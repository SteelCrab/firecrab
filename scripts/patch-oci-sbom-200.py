#!/usr/bin/env python3
from pathlib import Path

path = Path("firecrab-api/src/oci.rs")
text = path.read_text()

old = "mod ext4;\n"
new = "mod compliance;\n\nmod ext4;\n"
if text.count(old) != 1:
    raise SystemExit(f"module anchor count={text.count(old)}")
text = text.replace(old, new, 1)

old = '''    tracker.append_log(alias, "merging layers");
    let merged = merge_validated_layers(&validated, &scratch.join("rootfs")).await?;

    tracker.append_log(alias, "provisioning guest runtime");
'''
new = '''    tracker.append_log(alias, "merging layers");
    let merged = merge_validated_layers(&validated, &scratch.join("rootfs")).await?;

    tracker.append_log(alias, "detecting OCI package database");
    let sbom = match compliance::generate_spdx(
        merged.path(),
        alias,
        reference.version.as_str(),
        cached.resolved.architecture,
    ) {
        Ok(Some(document)) => {
            tracker.append_log(
                alias,
                format!(
                    "generated SPDX SBOM from {} ({} packages)",
                    document.package_manager, document.package_count
                ),
            );
            Some(document)
        }
        Ok(None) => {
            let warning =
                "warning: no apk/dpkg/rpm package database detected; importing without an SBOM";
            tracker.append_log(alias, warning);
            tracing::warn!(alias, "{warning}");
            None
        }
        Err(error) => {
            tracker.append_log(
                alias,
                format!("warning: OCI SBOM generation failed: {error}; importing without an SBOM"),
            );
            tracing::warn!(alias, error = %error, "OCI SBOM generation failed; continuing import");
            None
        }
    };

    tracker.append_log(alias, "provisioning guest runtime");
'''
if text.count(old) != 1:
    raise SystemExit(f"merge anchor count={text.count(old)}")
text = text.replace(old, new, 1)

old = '''    tracker.append_log(alias, "naming and registering template");
    let named = name_oci_image(bootable, reference, templates)?;
    register_named_oci_image(named, templates)?;
    Ok(())
'''
new = '''    tracker.append_log(alias, "naming template");
    let named = name_oci_image(bootable, reference, templates)?;

    let compliance_path = if let Some(document) = sbom.as_ref() {
        match compliance::write_spdx_bundle(
            image_root,
            alias,
            cached.resolved.architecture,
            document,
        ) {
            Ok(path) => {
                tracker.append_log(alias, format!("attached SPDX SBOM at {}", path.display()));
                Some(path)
            }
            Err(error) => {
                tracker.append_log(
                    alias,
                    format!("warning: could not persist OCI SBOM: {error}; continuing import"),
                );
                tracing::warn!(
                    alias,
                    error = %error,
                    "could not persist OCI SBOM; continuing import"
                );
                None
            }
        }
    } else {
        None
    };

    tracker.append_log(alias, "registering template");
    if let Err(error) = register_named_oci_image(named, templates) {
        if compliance_path.is_some() {
            compliance::remove_bundle(image_root, alias, cached.resolved.architecture);
        }
        return Err(error);
    }
    Ok(())
'''
if text.count(old) != 1:
    raise SystemExit(f"register anchor count={text.count(old)}")
text = text.replace(old, new, 1)

path.write_text(text)
