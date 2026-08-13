use super::*;

use std::collections::{BTreeMap, HashSet};
use std::ffi::{CString, OsString};
use std::fs::{self, File, OpenOptions};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::sync::atomic::{AtomicU8, Ordering};

const WHITEOUT_PREFIX: &[u8] = b".wh.";
const OPAQUE_WHITEOUT: &[u8] = b".wh..wh..opq";
// The merge tree is host-visible and service-owned. Preserve ordinary and
// sticky permissions, but never activate attacker-supplied set-ID bits on it.
const HOST_SAFE_MODE_MASK: u32 = 0o1777;
const MERGE_ACTIVE: u8 = 0;
const MERGE_PUBLISHING: u8 = 1;
const MERGE_CANCELLED: u8 = 2;
const MERGE_FINISHED: u8 = 3;

struct MergeControl {
    state: std::sync::Arc<AtomicU8>,
    destination: PathBuf,
}

impl MergeControl {
    fn check(&self) -> Result<(), ResolveError> {
        if self.state.load(Ordering::Acquire) == MERGE_ACTIVE {
            Ok(())
        } else {
            Err(ResolveError::MergeCancelled {
                path: self.destination.clone(),
            })
        }
    }

    fn begin_publish(&self) -> Result<(), ResolveError> {
        self.state
            .compare_exchange(
                MERGE_ACTIVE,
                MERGE_PUBLISHING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|_| ResolveError::MergeCancelled {
                path: self.destination.clone(),
            })
    }

    fn finish(&self) {
        self.state.store(MERGE_FINISHED, Ordering::Release);
    }
}

struct CancelMergeOnDrop {
    state: std::sync::Arc<AtomicU8>,
    armed: bool,
}

impl Drop for CancelMergeOnDrop {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.state.compare_exchange(
                MERGE_ACTIVE,
                MERGE_CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }
}

#[derive(Debug)]
enum Whiteout {
    Remove(PathBuf),
    Opaque(PathBuf),
}

#[derive(Debug, Clone)]
struct DirectoryMetadata {
    path: PathBuf,
    mode: u32,
    mtime: i64,
}

#[derive(Debug, Clone)]
struct PlannedHardlink {
    path: PathBuf,
    target: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlannedEntryKind {
    Whiteout,
    Directory,
    Other,
}

#[derive(Debug)]
struct LayerPlan {
    whiteouts: Vec<Whiteout>,
    directories: Vec<DirectoryMetadata>,
    hardlinks: Vec<PlannedHardlink>,
}

pub(super) async fn merge_validated_layers(
    layers: &[ValidatedLayer],
    destination: &Path,
) -> Result<MergedRootfs, ResolveError> {
    let layers = layers.to_vec();
    let destination = destination.to_owned();
    let join_path = destination.clone();
    let state = std::sync::Arc::new(AtomicU8::new(MERGE_ACTIVE));
    let worker_control = MergeControl {
        state: state.clone(),
        destination: destination.clone(),
    };
    let mut cancel_on_drop = CancelMergeOnDrop { state, armed: true };
    let result = tokio::task::spawn_blocking(move || {
        merge_layers_blocking(&layers, &destination, &worker_control)
    })
    .await
    .map_err(|error| merge_io("join worker", join_path, io::Error::other(error)));
    cancel_on_drop.armed = false;
    result?
}

fn merge_layers_blocking(
    layers: &[ValidatedLayer],
    destination: &Path,
    control: &MergeControl,
) -> Result<MergedRootfs, ResolveError> {
    control.check()?;
    let parent = destination.parent().ok_or_else(|| {
        merge_io(
            "resolve destination parent",
            destination.to_owned(),
            io::Error::new(io::ErrorKind::InvalidInput, "destination has no parent"),
        )
    })?;
    if destination.file_name().is_none() {
        return Err(merge_io(
            "resolve destination name",
            destination.to_owned(),
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "destination must name a directory below its parent",
            ),
        ));
    }
    fs::create_dir_all(parent)
        .map_err(|error| merge_io("create destination parent", parent.to_owned(), error))?;
    match fs::symlink_metadata(destination) {
        Ok(_) => {
            return Err(ResolveError::MergeDestinationExists {
                path: destination.to_owned(),
            });
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(merge_io(
                "inspect destination",
                destination.to_owned(),
                error,
            ));
        }
    }

    let partial = parent.join(format!(".firecrab-oci-merge-{}.partial", Uuid::new_v4()));
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder
        .create(&partial)
        .map_err(|error| merge_io("create partial tree", partial.clone(), error))?;
    let mut cleanup = PartialTreeCleanup::new(partial.clone());
    let tree = partial.join("rootfs");
    let mut tree_builder = fs::DirBuilder::new();
    tree_builder.mode(0o700);
    tree_builder
        .create(&tree)
        .map_err(|error| merge_io("create private root tree", tree.clone(), error))?;
    let mut directory_metadata = BTreeMap::new();

    let result = (|| {
        for layer in layers {
            control.check()?;
            apply_layer(layer, &tree, &mut directory_metadata, control)?;
        }
        apply_directory_metadata(&tree, &directory_metadata, control)?;
        control.begin_publish()?;
        let publish_result = publish_tree(&tree, destination);
        control.finish();
        publish_result?;
        Ok(MergedRootfs {
            path: destination.to_owned(),
        })
    })();

    if let Err(cleanup_error) = cleanup.remove_now_if_needed() {
        tracing::warn!(
            error = %cleanup_error,
            path = %partial.display(),
            "failed to remove partial OCI layer merge tree"
        );
    }
    result
}

fn apply_layer(
    layer: &ValidatedLayer,
    root: &Path,
    directory_metadata: &mut BTreeMap<PathBuf, DirectoryMetadata>,
    control: &MergeControl,
) -> Result<(), ResolveError> {
    let digest = &layer.layer.source.descriptor.digest;
    let mut file = open_verified_layer(layer)?;
    let plan = plan_layer(&mut file, &layer.layer.path, digest, control)?;

    for whiteout in &plan.whiteouts {
        control.check()?;
        match whiteout {
            Whiteout::Remove(path) => {
                remove_merged_path(root, path, digest)?;
                forget_directory_metadata(directory_metadata, path, true);
            }
            Whiteout::Opaque(path) => {
                clear_merged_directory(root, path, digest)?;
                forget_directory_metadata(directory_metadata, path, false);
            }
        }
    }

    let mut directories = plan.directories.clone();
    directories.sort_by(|left, right| {
        path_depth(&left.path)
            .cmp(&path_depth(&right.path))
            .then_with(|| left.path.cmp(&right.path))
    });
    for metadata in directories {
        control.check()?;
        prepare_directory(root, &metadata.path, digest, directory_metadata)?;
        directory_metadata.insert(metadata.path.clone(), metadata);
    }

    extract_non_directory_entries(
        &mut file,
        &layer.layer.path,
        root,
        digest,
        directory_metadata,
        control,
    )?;
    apply_hardlinks(root, &plan.hardlinks, digest, directory_metadata, control)
}

fn open_verified_layer(layer: &ValidatedLayer) -> Result<File, ResolveError> {
    let descriptor = &layer.layer.source.descriptor;
    let path = &layer.layer.path;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    let mut file = options
        .open(path)
        .map_err(|error| merge_io("open validated layer", path.clone(), error))?;
    let metadata = file
        .metadata()
        .map_err(|error| merge_io("inspect validated layer", path.clone(), error))?;
    if !metadata.file_type().is_file() {
        return Err(merge_io(
            "inspect validated layer",
            path.clone(),
            io::Error::new(
                io::ErrorKind::InvalidData,
                "layer cache entry is not a file",
            ),
        ));
    }
    if metadata.len() != layer.layer.size {
        return Err(ResolveError::SizeMismatch {
            subject: format!("decompressed OCI layer {}", descriptor.digest),
            expected: layer.layer.size,
            actual: metadata.len(),
        });
    }

    let (size, actual) = hash_open_file(&mut file, path)?;
    if size != layer.layer.size {
        return Err(ResolveError::SizeMismatch {
            subject: format!("decompressed OCI layer {}", descriptor.digest),
            expected: layer.layer.size,
            actual: size,
        });
    }
    if actual != layer.layer.diff_id {
        return Err(ResolveError::DiffIdMismatch {
            compressed_digest: descriptor.digest.clone(),
            expected: layer.layer.diff_id.clone(),
            actual,
        });
    }
    validate_layer_archive_file(&mut file, path, &descriptor.digest)?;
    Ok(file)
}

fn hash_open_file(file: &mut File, path: &Path) -> Result<(u64, Sha256Digest), ResolveError> {
    file.seek(io::SeekFrom::Start(0))
        .map_err(|error| merge_io("rewind validated layer", path.to_owned(), error))?;
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| merge_io("hash validated layer", path.to_owned(), error))?;
        if read == 0 {
            break;
        }
        size = size.checked_add(read as u64).ok_or_else(|| {
            merge_io(
                "hash validated layer",
                path.to_owned(),
                io::Error::other("layer byte count overflow"),
            )
        })?;
        hasher.update(&buffer[..read]);
    }
    file.seek(io::SeekFrom::Start(0))
        .map_err(|error| merge_io("rewind validated layer", path.to_owned(), error))?;
    Ok((
        size,
        Sha256Digest(format!("sha256:{:x}", hasher.finalize())),
    ))
}

fn plan_layer(
    file: &mut File,
    layer_path: &Path,
    digest: &Sha256Digest,
    control: &MergeControl,
) -> Result<LayerPlan, ResolveError> {
    file.seek(io::SeekFrom::Start(0))
        .map_err(|error| merge_io("rewind layer for planning", layer_path.to_owned(), error))?;
    let mut archive = tar::Archive::new(&mut *file);
    let entries = archive
        .entries()
        .map_err(|error| malformed_layer_archive(digest, error))?;
    let mut seen = HashSet::new();
    let mut path_kinds = BTreeMap::new();
    let mut whiteouts = Vec::new();
    let mut directories = Vec::new();
    let mut hardlinks = Vec::new();

    for (index, entry) in entries.enumerate() {
        control.check()?;
        let entry = entry.map_err(|error| {
            malformed_layer_archive(
                digest,
                format!("could not plan member {}: {error}", index + 1),
            )
        })?;
        let entry_type = entry.header().entry_type();
        if entry_type.is_pax_global_extensions() {
            continue;
        }
        let path = normalized_entry_path(&entry, digest, index + 1)?;
        if !seen.insert(path.clone()) {
            return Err(unsafe_tar_member(
                digest,
                path,
                TarMemberViolation::DuplicatePath,
            ));
        }
        if let Some(whiteout) = classify_whiteout(&entry, &path, digest)? {
            path_kinds.insert(path.clone(), PlannedEntryKind::Whiteout);
            whiteouts.push(whiteout);
        } else if entry_type.is_dir() {
            path_kinds.insert(path.clone(), PlannedEntryKind::Directory);
            let mode = entry.header().mode().map_err(|error| {
                malformed_layer_archive(
                    digest,
                    format!(
                        "member {} has an invalid directory mode: {error}",
                        index + 1
                    ),
                )
            })?;
            let mtime = entry.header().mtime().map_err(|error| {
                malformed_layer_archive(
                    digest,
                    format!(
                        "member {} has an invalid directory mtime: {error}",
                        index + 1
                    ),
                )
            })?;
            let mtime = i64::try_from(mtime).map_err(|_| {
                malformed_layer_archive(
                    digest,
                    format!("member {} directory mtime is out of range", index + 1),
                )
            })?;
            directories.push(DirectoryMetadata { path, mode, mtime });
        } else if entry_type.is_hard_link() {
            path_kinds.insert(path.clone(), PlannedEntryKind::Other);
            let target = entry
                .link_name()
                .map_err(|error| {
                    malformed_layer_archive(
                        digest,
                        format!(
                            "could not decode hard link target for member {}: {error}",
                            index + 1
                        ),
                    )
                })?
                .map(|target| target.into_owned())
                .ok_or_else(|| {
                    unsafe_tar_member(
                        digest,
                        path.clone(),
                        TarMemberViolation::MissingHardlinkTarget,
                    )
                })?;
            let target = normalize_safe_path(&target).ok_or_else(|| {
                unsafe_tar_member(
                    digest,
                    path.clone(),
                    TarMemberViolation::HardlinkTarget { target },
                )
            })?;
            hardlinks.push(PlannedHardlink { path, target });
        } else {
            path_kinds.insert(path, PlannedEntryKind::Other);
        }
    }

    for path in path_kinds.keys() {
        for ancestor in path.ancestors().skip(1) {
            if ancestor.as_os_str().is_empty() {
                break;
            }
            if path_kinds
                .get(ancestor)
                .is_some_and(|kind| *kind != PlannedEntryKind::Directory)
            {
                return Err(unsafe_tar_member(
                    digest,
                    ancestor.to_owned(),
                    TarMemberViolation::ConflictingPath {
                        descendant: path.clone(),
                    },
                ));
            }
        }
    }

    file.seek(io::SeekFrom::Start(0))
        .map_err(|error| merge_io("rewind planned layer", layer_path.to_owned(), error))?;
    Ok(LayerPlan {
        whiteouts,
        directories,
        hardlinks,
    })
}

fn normalized_entry_path<R: io::Read>(
    entry: &tar::Entry<'_, R>,
    digest: &Sha256Digest,
    index: usize,
) -> Result<PathBuf, ResolveError> {
    let path = entry.path().map_err(|error| {
        malformed_layer_archive(
            digest,
            format!("could not decode member {index} path while planning: {error}"),
        )
    })?;
    normalize_layer_path(&path, entry.header().entry_type().is_dir())
        .ok_or_else(|| unsafe_tar_member(digest, path.into_owned(), TarMemberViolation::Path))
}

fn normalize_safe_path(path: &Path) -> Option<PathBuf> {
    normalize_layer_path(path, false)
}

fn normalize_layer_path(path: &Path, allow_root: bool) -> Option<PathBuf> {
    if !(is_safe_layer_member_path(path) || allow_root && is_layer_root_path(path)) {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        if let std::path::Component::Normal(component) = component {
            normalized.push(component);
        }
    }
    (allow_root || !normalized.as_os_str().is_empty()).then_some(normalized)
}

fn classify_whiteout<R: io::Read>(
    entry: &tar::Entry<'_, R>,
    path: &Path,
    digest: &Sha256Digest,
) -> Result<Option<Whiteout>, ResolveError> {
    let Some(basename) = path.file_name().map(|name| name.as_bytes()) else {
        return Ok(None);
    };
    if !basename.starts_with(WHITEOUT_PREFIX) {
        return Ok(None);
    }
    if !entry.header().entry_type().is_file() {
        return Err(unsafe_tar_member(
            digest,
            path.to_owned(),
            TarMemberViolation::InvalidWhiteoutType,
        ));
    }
    if entry.size() != 0 {
        return Err(unsafe_tar_member(
            digest,
            path.to_owned(),
            TarMemberViolation::NonEmptyWhiteout { size: entry.size() },
        ));
    }
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    if basename == OPAQUE_WHITEOUT {
        return Ok(Some(Whiteout::Opaque(parent.to_owned())));
    }
    let target_name = &basename[WHITEOUT_PREFIX.len()..];
    if target_name.is_empty() || matches!(target_name, b"." | b"..") {
        return Err(unsafe_tar_member(
            digest,
            path.to_owned(),
            TarMemberViolation::InvalidWhiteoutTarget,
        ));
    }
    let mut target = parent.to_owned();
    target.push(OsString::from_vec(target_name.to_vec()));
    Ok(Some(Whiteout::Remove(target)))
}

fn extract_non_directory_entries(
    file: &mut File,
    layer_path: &Path,
    root: &Path,
    digest: &Sha256Digest,
    directory_metadata: &mut BTreeMap<PathBuf, DirectoryMetadata>,
    control: &MergeControl,
) -> Result<(), ResolveError> {
    file.seek(io::SeekFrom::Start(0))
        .map_err(|error| merge_io("rewind layer for extraction", layer_path.to_owned(), error))?;
    let mut archive = tar::Archive::new(&mut *file);
    archive.set_preserve_permissions(false);
    archive.set_preserve_ownerships(false);
    archive.set_unpack_xattrs(false);
    archive.set_overwrite(false);
    let entries = archive
        .entries()
        .map_err(|error| malformed_layer_archive(digest, error))?;
    for (index, entry) in entries.enumerate() {
        control.check()?;
        let mut entry = entry.map_err(|error| {
            malformed_layer_archive(
                digest,
                format!("could not extract member {}: {error}", index + 1),
            )
        })?;
        let entry_type = entry.header().entry_type();
        if entry_type.is_pax_global_extensions() {
            continue;
        }
        let path = normalized_entry_path(&entry, digest, index + 1)?;
        if classify_whiteout(&entry, &path, digest)?.is_some()
            || entry_type.is_dir()
            || entry_type.is_hard_link()
        {
            continue;
        }
        let mode = entry.header().mode().map_err(|error| {
            malformed_layer_archive(
                digest,
                format!("member {} has an invalid mode: {error}", index + 1),
            )
        })?;
        ensure_parent_directories(root, &path, digest)?;
        remove_existing_target(root, &path, digest)?;
        forget_directory_metadata(directory_metadata, &path, true);
        let unpacked = entry
            .unpack_in(root)
            .map_err(|error| merge_io("extract member", path.clone(), error))?;
        if !unpacked {
            return Err(unsafe_tar_member(digest, path, TarMemberViolation::Path));
        }
        if entry_type.is_file() {
            let destination = root.join(&path);
            fs::set_permissions(
                &destination,
                fs::Permissions::from_mode(mode & HOST_SAFE_MODE_MASK),
            )
            .map_err(|error| merge_io("set merged file permissions", destination, error))?;
        }
    }
    Ok(())
}

fn prepare_directory(
    root: &Path,
    path: &Path,
    digest: &Sha256Digest,
    directory_metadata: &mut BTreeMap<PathBuf, DirectoryMetadata>,
) -> Result<(), ResolveError> {
    ensure_parent_directories(root, path, digest)?;
    let destination = root.join(path);
    match fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        Ok(_) => {
            remove_existing_target(root, path, digest)?;
            forget_directory_metadata(directory_metadata, path, true);
            create_merged_directory(&destination)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_merged_directory(&destination)
        }
        Err(error) => Err(merge_io("inspect directory target", destination, error)),
    }
}

fn ensure_parent_directories(
    root: &Path,
    path: &Path,
    digest: &Sha256Digest,
) -> Result<(), ResolveError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let mut relative = PathBuf::new();
    for component in parent.components() {
        let std::path::Component::Normal(component) = component else {
            return Err(unsafe_tar_member(
                digest,
                path.to_owned(),
                TarMemberViolation::Path,
            ));
        };
        relative.push(component);
        let candidate = root.join(&relative);
        match fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(unsafe_tar_member(
                    digest,
                    path.to_owned(),
                    TarMemberViolation::SymlinkAncestor { ancestor: relative },
                ));
            }
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => {
                return Err(unsafe_tar_member(
                    digest,
                    path.to_owned(),
                    TarMemberViolation::NonDirectoryAncestor { ancestor: relative },
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                create_merged_directory(&candidate)?;
            }
            Err(error) => return Err(merge_io("inspect path ancestor", candidate, error)),
        }
    }
    Ok(())
}

fn existing_parent_is_safe(
    root: &Path,
    path: &Path,
    digest: &Sha256Digest,
) -> Result<bool, ResolveError> {
    let Some(parent) = path.parent() else {
        return Ok(true);
    };
    let mut relative = PathBuf::new();
    for component in parent.components() {
        let std::path::Component::Normal(component) = component else {
            return Err(unsafe_tar_member(
                digest,
                path.to_owned(),
                TarMemberViolation::Path,
            ));
        };
        relative.push(component);
        let candidate = root.join(&relative);
        match fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(unsafe_tar_member(
                    digest,
                    path.to_owned(),
                    TarMemberViolation::SymlinkAncestor { ancestor: relative },
                ));
            }
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => {
                return Err(unsafe_tar_member(
                    digest,
                    path.to_owned(),
                    TarMemberViolation::NonDirectoryAncestor { ancestor: relative },
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(merge_io("inspect path ancestor", candidate, error)),
        }
    }
    Ok(true)
}

fn remove_merged_path(root: &Path, path: &Path, digest: &Sha256Digest) -> Result<(), ResolveError> {
    if !lower_parent_is_directory(root, path, digest)? {
        return Ok(());
    }
    let destination = root.join(path);
    match fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.file_type().is_dir() => fs::remove_dir_all(&destination)
            .map_err(|error| merge_io("remove whiteout directory", destination, error)),
        Ok(_) => fs::remove_file(&destination)
            .map_err(|error| merge_io("remove whiteout path", destination, error)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(merge_io("inspect whiteout target", destination, error)),
    }
}

fn lower_parent_is_directory(
    root: &Path,
    path: &Path,
    digest: &Sha256Digest,
) -> Result<bool, ResolveError> {
    let Some(parent) = path.parent() else {
        return Ok(true);
    };
    let mut relative = PathBuf::new();
    for component in parent.components() {
        let std::path::Component::Normal(component) = component else {
            return Err(unsafe_tar_member(
                digest,
                path.to_owned(),
                TarMemberViolation::Path,
            ));
        };
        relative.push(component);
        match fs::symlink_metadata(root.join(&relative)) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(merge_io(
                    "inspect whiteout ancestor",
                    root.join(relative),
                    error,
                ));
            }
        }
    }
    Ok(true)
}

fn remove_existing_target(
    root: &Path,
    path: &Path,
    digest: &Sha256Digest,
) -> Result<(), ResolveError> {
    if !existing_parent_is_safe(root, path, digest)? {
        return Ok(());
    }
    let destination = root.join(path);
    match fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.file_type().is_dir() => fs::remove_dir_all(&destination)
            .map_err(|error| merge_io("remove merged directory", destination, error)),
        Ok(_) => fs::remove_file(&destination)
            .map_err(|error| merge_io("remove merged path", destination, error)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(merge_io("inspect merged path", destination, error)),
    }
}

fn clear_merged_directory(
    root: &Path,
    path: &Path,
    digest: &Sha256Digest,
) -> Result<(), ResolveError> {
    if !lower_parent_is_directory(root, path, digest)? {
        return Ok(());
    }
    let directory = root.join(path);
    let metadata = match fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(merge_io("inspect opaque directory", directory, error)),
    };
    if !metadata.file_type().is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(&directory)
        .map_err(|error| merge_io("read opaque directory", directory.clone(), error))?
    {
        let entry =
            entry.map_err(|error| merge_io("read opaque child", directory.clone(), error))?;
        let child = entry.path();
        let metadata = fs::symlink_metadata(&child)
            .map_err(|error| merge_io("inspect opaque child", child.clone(), error))?;
        if metadata.file_type().is_dir() {
            fs::remove_dir_all(&child)
                .map_err(|error| merge_io("remove opaque directory", child, error))?;
        } else {
            fs::remove_file(&child)
                .map_err(|error| merge_io("remove opaque child", child, error))?;
        }
    }
    Ok(())
}

fn apply_hardlinks(
    root: &Path,
    hardlinks: &[PlannedHardlink],
    digest: &Sha256Digest,
    directory_metadata: &mut BTreeMap<PathBuf, DirectoryMetadata>,
    control: &MergeControl,
) -> Result<(), ResolveError> {
    for hardlink in hardlinks {
        control.check()?;
        ensure_parent_directories(root, &hardlink.path, digest)?;
        remove_existing_target(root, &hardlink.path, digest)?;
        forget_directory_metadata(directory_metadata, &hardlink.path, true);
    }

    let mut pending = hardlinks.to_vec();
    while !pending.is_empty() {
        let mut unresolved = Vec::new();
        let mut progressed = false;
        for hardlink in pending {
            control.check()?;
            if !existing_parent_is_safe(root, &hardlink.target, digest)? {
                unresolved.push(hardlink);
                continue;
            }
            let source = root.join(&hardlink.target);
            let metadata = match fs::symlink_metadata(&source) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    unresolved.push(hardlink);
                    continue;
                }
                Err(error) => return Err(merge_io("inspect hard link target", source, error)),
            };
            if metadata.file_type().is_dir() {
                return Err(unsafe_tar_member(
                    digest,
                    hardlink.path,
                    TarMemberViolation::DirectoryHardlinkTarget {
                        target: hardlink.target,
                    },
                ));
            }
            ensure_parent_directories(root, &hardlink.path, digest)?;
            let destination = root.join(&hardlink.path);
            fs::hard_link(&source, &destination)
                .map_err(|error| merge_io("create hard link", destination, error))?;
            progressed = true;
        }
        if !progressed {
            let hardlink = unresolved
                .into_iter()
                .next()
                .expect("a non-empty pending list leaves an unresolved link");
            return Err(unsafe_tar_member(
                digest,
                hardlink.path,
                TarMemberViolation::MissingMergedHardlinkTarget {
                    target: hardlink.target,
                },
            ));
        }
        pending = unresolved;
    }
    Ok(())
}

fn apply_directory_metadata(
    root: &Path,
    metadata: &BTreeMap<PathBuf, DirectoryMetadata>,
    control: &MergeControl,
) -> Result<(), ResolveError> {
    let has_root_metadata = metadata.contains_key(Path::new(""));
    let mut metadata: Vec<&DirectoryMetadata> = metadata.values().collect();
    metadata.sort_by(|left, right| {
        path_depth(&right.path)
            .cmp(&path_depth(&left.path))
            .then_with(|| left.path.cmp(&right.path))
    });
    for metadata in metadata {
        control.check()?;
        let destination = root.join(&metadata.path);
        let current = fs::symlink_metadata(&destination)
            .map_err(|error| merge_io("inspect final directory", destination.clone(), error))?;
        if !current.file_type().is_dir() {
            return Err(merge_io(
                "inspect final directory",
                destination,
                io::Error::new(io::ErrorKind::InvalidData, "directory entry was replaced"),
            ));
        }
        fs::set_permissions(
            &destination,
            fs::Permissions::from_mode(metadata.mode & HOST_SAFE_MODE_MASK),
        )
        .map_err(|error| merge_io("set directory permissions", destination.clone(), error))?;
        let mtime = filetime::FileTime::from_unix_time(metadata.mtime, 0);
        filetime::set_file_mtime(&destination, mtime)
            .map_err(|error| merge_io("set directory mtime", destination, error))?;
    }
    if !has_root_metadata {
        control.check()?;
        fs::set_permissions(root, fs::Permissions::from_mode(0o755))
            .map_err(|error| merge_io("set default root permissions", root.to_owned(), error))?;
    }
    Ok(())
}

fn forget_directory_metadata(
    metadata: &mut BTreeMap<PathBuf, DirectoryMetadata>,
    path: &Path,
    include_path: bool,
) {
    metadata.retain(|candidate, _| {
        !(candidate.starts_with(path) && (include_path || candidate != path))
    });
}

fn create_merged_directory(path: &Path) -> Result<(), ResolveError> {
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o755);
    builder
        .create(path)
        .map_err(|error| merge_io("create merged directory", path.to_owned(), error))
}

fn path_depth(path: &Path) -> usize {
    path.components().count()
}

fn publish_tree(partial: &Path, destination: &Path) -> Result<(), ResolveError> {
    let source = CString::new(partial.as_os_str().as_bytes()).map_err(|error| {
        merge_io(
            "encode partial path",
            partial.to_owned(),
            io::Error::new(io::ErrorKind::InvalidInput, error),
        )
    })?;
    let target = CString::new(destination.as_os_str().as_bytes()).map_err(|error| {
        merge_io(
            "encode destination path",
            destination.to_owned(),
            io::Error::new(io::ErrorKind::InvalidInput, error),
        )
    })?;
    // libc does not expose renameat2 on every Linux libc (notably musl), but
    // Firecrab's release architectures all provide the kernel syscall.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::AlreadyExists {
        return Err(ResolveError::MergeDestinationExists {
            path: destination.to_owned(),
        });
    }
    Err(merge_io(
        "publish partial tree",
        destination.to_owned(),
        error,
    ))
}

fn merge_io(operation: &'static str, path: PathBuf, source: io::Error) -> ResolveError {
    ResolveError::MergeIo {
        operation,
        path,
        source,
    }
}

struct PartialTreeCleanup {
    path: PathBuf,
    published: bool,
}

impl PartialTreeCleanup {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            published: false,
        }
    }

    fn remove_now_if_needed(&mut self) -> Result<(), ResolveError> {
        if self.published {
            return Ok(());
        }
        make_tree_owner_accessible(&self.path);
        match fs::remove_dir_all(&self.path) {
            Ok(()) => {
                self.published = true;
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.published = true;
                Ok(())
            }
            Err(error) => Err(merge_io("remove partial tree", self.path.clone(), error)),
        }
    }
}

impl Drop for PartialTreeCleanup {
    fn drop(&mut self) {
        if !self.published {
            make_tree_owner_accessible(&self.path);
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn make_tree_owner_accessible(path: &Path) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if !metadata.file_type().is_dir() {
        return;
    }
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let child = entry.path();
        if fs::symlink_metadata(&child)
            .map(|metadata| metadata.file_type().is_dir())
            .unwrap_or(false)
        {
            make_tree_owner_accessible(&child);
        }
    }
}
