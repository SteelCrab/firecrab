# M2Image 옵션 메뉴 + 구운 패키지 삭제 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 상세 패널의 굽기/설치/삭제 3개 버튼을 "⋯" 옵션 메뉴 안 4개 항목(굽기·설치·삭제·굽기삭제)으로 재구성하고, 스테이징만 되고 아직 설치되지 않은 로컬 패키지를 지울 수 있는 백엔드 엔드포인트를 신설한다.

**Architecture:** 백엔드에 `DELETE /api/images/{alias}/package` 하나만 추가(기존 GET/POST 라우트에 얹음). 프론트는 `firecrab-frontend/src/components/Images.tsx`에 이 프로젝트 최초의 드롭다운 패턴인 `OptionsMenu` 컴포넌트를 신설하고, `ImageDetail`의 기존 3버튼 `package-row`를 이 메뉴로 교체한다. 굽기·설치·삭제 3개 항목은 지금 코드의 disabled/라벨 로직을 그대로 옮기고, "굽기삭제" 항목만 신규 — 상태에 따라 기존 `cancelBootstrap`(지금까지 미사용) 또는 신규 `deleteStagedPackage`를 호출한다.

**Tech Stack:** Rust + Axum(백엔드), React 19 + TypeScript strict(프론트, `noUnusedLocals`/`noUnusedParameters` 켜짐). 프론트 테스트 프레임워크 없음 — `npm run build`/`npm run lint`로만 검증.

## Global Constraints

- 백엔드: `firecrab-api/src/handlers/images.rs`, `firecrab-api/src/server.rs`만 수정한다. 새 wire 타입은 필요 없다(요청/응답 바디 없는 DELETE).
- 프론트: `firecrab-frontend/src/components/Images.tsx`, `firecrab-frontend/src/api/client.ts`, `firecrab-frontend/src/index.css`만 수정한다.
- 다크모드 대응 불필요 — 이 앱은 `prefers-color-scheme`/`data-theme`가 없는 단일 테마다.
- 메뉴 항목 4개는 항상 다 보이고, 지금 상황에 안 맞는 항목은 숨기지 않고 회색+비활성으로 보여준다.
- 각 태스크가 끝난 시점에 백엔드는 `cargo test -p firecrab-api`(해당 모듈), 프론트는 `npm run build`+`npm run lint`가 반드시 통과해야 한다.
- 커밋 정책: 격리된 SDD worktree에서 실행 중이면 각 태스크 끝에 실제로 `git commit`한다(리뷰/수정 루프가 diff 비교에 커밋을 필요로 함). 사용자의 실제 작업 브랜치에서 직접 실행 중이면 커밋하지 말고 스테이징 후 권장 메시지만 제시한다 — 실행 시작 시점의 환경에 맞는 쪽을 따른다.

---

## 사전 지식 — 지금 코드의 관련 부분

**백엔드** (`firecrab-api/src/handlers/images.rs`):
- `delete_image`(278-374줄)가 이미 있는 삭제 핸들러의 정확한 스타일 — alias 검증 → 진행 중 작업 가드 → 실제 삭제 → `StatusCode::NO_CONTENT`. 새 핸들러가 그대로 미러링한다.
- `get_image_package`(125-147줄 부근)의 alias 검증(`TemplateRegistry::known_spec(&alias).is_none() && state.templates.resolve_alias(&alias).is_none()` → 404)을 새 핸들러도 그대로 쓴다.
- `image_install::staged_package_path(image_root, alias)` — `.packages/{alias}.tar.zst` 경로를 돌려준다. `image_install`은 이미 파일 맨 위에 `use crate::image_install;`로 임포트돼 있다.
- `state.image_packages`/`state.image_installs`는 둘 다 `ImageInstallTracker` 타입이고 `is_running(&alias) -> bool` 메서드가 있다.
- 테스트 모듈(379줄~)의 `empty_state(root)` 헬퍼 — `TemplateRegistry::from_specs(root, std::iter::empty())`로 아무 템플릿도 등록 안 된 상태를 만든다. `TemplateRegistry::known_specs()`는 등록 여부와 무관하게 alpine-3.24/ubuntu-26.04/rocky-9를 항상 포함하므로, `empty_state`에서도 이 3개는 "known_spec"으로 인식된다(`list_images_includes_not_installed_known_aliases` 테스트가 이미 이 사실에 의존).
- 테스트에서 `RequestId`는 항상 `Extension(RequestId(uuid::Uuid::nil()))`로 만든다(파일 전체 관례).

**프론트** (`firecrab-frontend/src/components/Images.tsx`):
- `ImageDetail`(177줄부터)의 현재 `package-row`(263-328줄 부근)가 굽기/설치/삭제 3버튼을 렌더링한다. 이 블록 전체가 이번에 사라지고 `OptionsMenu` 호출로 바뀐다.
- `api/client.ts`의 `cancelBootstrap(bootstrapId)`(295-305줄)는 이미 구현돼 있지만 `Images.tsx`에서 지금까지 한 번도 import된 적이 없다.
- `deleteImage(alias)`(client.ts 175-185줄)가 새 `deleteStagedPackage` 함수가 그대로 미러링할 정확한 모양이다.

---

### Task 1: 백엔드 — `DELETE /api/images/{alias}/package` 엔드포인트

**Files:**
- Modify: `firecrab-api/src/handlers/images.rs` (핸들러 + 테스트 4개 추가)
- Modify: `firecrab-api/src/server.rs` (라우트에 `.delete(...)` 추가)

**Interfaces:**
- Produces: `pub async fn delete_staged_package(State<AppState>, Path<String>, Extension<RequestId>) -> Result<StatusCode, AppError>` — Task 3(프론트)가 호출하는 `DELETE /api/images/{alias}/package`의 실제 구현. 새 `AppError` 코드 2개: `"package_in_progress"`, `"not_staged"`(둘 다 409).

- [ ] **Step 1: 핸들러 추가**

`firecrab-api/src/handlers/images.rs`에서 `delete_image` 함수(278-374줄) 바로 다음, `use axum::Extension;`(376줄) 앞에 추가:

```rust
/// `DELETE /api/images/{alias}/package` — deletes a staged-but-not-installed
/// local package (`.packages/{alias}.tar.zst`). Independent of `delete_image`
/// (which only ever acts on an *installed* template): a staged package can be
/// deleted even for an alias that's still installed, since the staged archive
/// only feeds a future (re)install and isn't itself load-bearing.
pub async fn delete_staged_package(
    State(state): State<AppState>,
    Path(alias): Path<String>,
    Extension(request_id): Extension<RequestId>,
) -> Result<StatusCode, AppError> {
    // Same alias validation as `get_image_package`.
    if TemplateRegistry::known_spec(&alias).is_none()
        && state.templates.resolve_alias(&alias).is_none()
    {
        return Err(AppError::not_found(request_id.0));
    }

    // A download/verify or an install-from-staged may be reading or writing
    // this exact file right now.
    if state.image_packages.is_running(&alias) || state.image_installs.is_running(&alias) {
        return Err(AppError::conflict(
            "package_in_progress",
            "cannot delete while a package download or install is running for this template",
            request_id.0,
        ));
    }

    let path = image_install::staged_package_path(state.templates.image_root_path(), &alias);
    if !path.is_file() {
        return Err(AppError::conflict(
            "not_staged",
            "no staged package exists for this alias",
            request_id.0,
        ));
    }

    if let Err(error) = tokio::fs::remove_file(&path).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(AppError::internal(request_id.0));
        }
    }

    Ok(StatusCode::NO_CONTENT)
}
```

- [ ] **Step 2: 라우트에 DELETE 추가**

`firecrab-api/src/server.rs`에서 (현재 206-209줄 부근):

```rust
        .route(
            "/api/images/{alias}/package",
            get(handlers::images::get_image_package).post(handlers::images::start_image_package),
        )
```

다음으로 교체:

```rust
        .route(
            "/api/images/{alias}/package",
            get(handlers::images::get_image_package)
                .post(handlers::images::start_image_package)
                .delete(handlers::images::delete_staged_package),
        )
```

- [ ] **Step 3: 테스트 4개 추가**

`firecrab-api/src/handlers/images.rs`의 `mod tests` 안, 아무 곳에나(예: 파일 맨 끝) 추가:

```rust
    #[tokio::test]
    async fn delete_staged_package_removes_the_staged_archive() {
        let directory = tempdir().unwrap();
        let state = empty_state(directory.path()).await;
        let staged = crate::image_install::staged_package_path(
            state.templates.image_root_path(),
            "ubuntu-26.04",
        );
        fs::create_dir_all(staged.parent().unwrap()).unwrap();
        fs::write(&staged, b"pretend tar.zst").unwrap();

        let status = delete_staged_package(
            State(state),
            Path("ubuntu-26.04".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await
        .unwrap();

        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(!staged.is_file());
    }

    #[tokio::test]
    async fn delete_staged_package_refuses_when_nothing_is_staged() {
        let directory = tempdir().unwrap();
        let state = empty_state(directory.path()).await;

        let error = delete_staged_package(
            State(state),
            Path("ubuntu-26.04".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn delete_staged_package_refuses_while_a_package_download_is_running() {
        let directory = tempdir().unwrap();
        let mut state = empty_state(directory.path()).await;
        state.image_packages = ImageInstallTracker::with_base_url("http://example");
        state.image_packages.begin("ubuntu-26.04").unwrap();
        let staged = crate::image_install::staged_package_path(
            state.templates.image_root_path(),
            "ubuntu-26.04",
        );
        fs::create_dir_all(staged.parent().unwrap()).unwrap();
        fs::write(&staged, b"pretend tar.zst").unwrap();

        let error = delete_staged_package(
            State(state),
            Path("ubuntu-26.04".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn delete_staged_package_unknown_alias_is_not_found() {
        let directory = tempdir().unwrap();
        let state = empty_state(directory.path()).await;

        let error = delete_staged_package(
            State(state),
            Path("no-such-alias".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);
    }
```

- [ ] **Step 4: 빌드 + 테스트 확인**

Run: `cargo build -p firecrab-api`
Expected: 에러 없음.

Run: `cargo test -p firecrab-api delete_staged_package`
Expected: 4개 테스트 모두 통과.

Run: `cargo fmt -p firecrab-api -- --check` (또는 `cargo fmt -p firecrab-api`로 바로 정리)
Expected: diff 없음.

- [ ] **Step 5: 커밋**

worktree 안이면:
```bash
git add firecrab-api/src/handlers/images.rs firecrab-api/src/server.rs
git commit -m "feat(api): add DELETE /api/images/{alias}/package for staged packages"
```
메인 브랜치에서 직접 실행 중이면 커밋하지 말고 위 파일을 스테이징한 뒤 같은 메시지를 제안만 한다.

---

### Task 2: 프론트 — `deleteStagedPackage` API 클라이언트 함수

**Files:**
- Modify: `firecrab-frontend/src/api/client.ts`

**Interfaces:**
- Consumes: Task 1의 `DELETE /api/images/{alias}/package`.
- Produces: `export async function deleteStagedPackage(alias: string): Promise<void>` — Task 3이 그대로 가져다 쓴다.

이 태스크는 순수 추가이고 다른 어떤 코드에서도 아직 호출하지 않는다 — `export`된 함수는 TypeScript의 `noUnusedLocals`가 미사용으로 잡지 않으므로(같은 파일의 `cancelBootstrap`도 지금까지 이 상태였다) 독립적으로 빌드된다.

- [ ] **Step 1: 함수 추가**

`firecrab-frontend/src/api/client.ts`에서 `deleteImage`(175-185줄) 바로 다음에 추가:

```ts
/** Delete a staged-but-not-installed package (`DELETE /api/images/{alias}/package`). */
export async function deleteStagedPackage(alias: string): Promise<void> {
  let response: Response;
  try {
    response = await fetch(`/api/images/${encodeURIComponent(alias)}/package`, { method: "DELETE" });
  } catch (error) {
    throw ApiClientError.transport(transportDetail(error));
  }
  if (!response.ok) {
    throw await fail(response);
  }
}
```

- [ ] **Step 2: 빌드 + 린트 확인**

Run: `cd firecrab-frontend && npm run build`
Expected: 에러 없음.

Run: `npm run lint`
Expected: 에러 0건.

- [ ] **Step 3: 커밋**

worktree 안이면:
```bash
git add firecrab-frontend/src/api/client.ts
git commit -m "feat(frontend): add deleteStagedPackage API client function"
```
메인 브랜치에서 직접 실행 중이면 스테이징 후 제안만 한다.

---

### Task 3: 프론트 — `OptionsMenu` 컴포넌트 + `ImageDetail` 메뉴 배선

**이 태스크가 한 번에 큰 이유:** `OptionsMenu` 정의, `cancelBootstrap`/`deleteStagedPackage` import, `ImageDetail`의 새 prop, `Images()`의 새 핸들러, 호출부 배선이 서로 맞물려 있다 — 하나만 적용하면 `noUnusedLocals`/`noUnusedParameters`(미사용 함수·import·파라미터)로 빌드가 깨진다. 이 태스크의 모든 스텝을 다 적용한 뒤에만 빌드가 통과하면 된다(스텝 사이 중간 상태는 안 깨져도 되고 깨져도 된다 — 이전 플랜에서 확립된 관례).

**Files:**
- Modify: `firecrab-frontend/src/index.css`
- Modify: `firecrab-frontend/src/components/Images.tsx`

**Interfaces:**
- Consumes: Task 2의 `deleteStagedPackage`, 기존 `cancelBootstrap`(둘 다 `../api/client`), 기존 `ImageDetail`의 모든 prop(`onStartBootstrap` 등, Task 1~4 이전 플랜에서 확정된 것들 — 이름/타입 변경 없음).
- Produces: 새 컴포넌트 `function OptionsMenu({ items }: { items: { label: string; onClick: () => void; disabled: boolean }[] })`. `ImageDetail`에 `onCancelBootstrap: (bootstrapId: string) => Promise<void>`, `onDeleteStagedPackage: (alias: string) => Promise<void>` prop 추가(최종 prop 목록). `Images()`에 `handleCancelBootstrap`, `handleDeleteStagedPackage` 함수 추가.

- [ ] **Step 1: CSS 추가**

`firecrab-frontend/src/index.css`에서 `.subpanel { ... }` 블록(384-390줄) 바로 다음에 추가:

```css
.subpanel-header {
  display: flex;
  justify-content: flex-end;
}
.options-menu {
  position: relative;
}
.options-menu-trigger {
  border: none;
  background: transparent;
  color: var(--shell);
  font-size: 1.1rem;
  line-height: 1;
  padding: 0.3rem 0.5rem;
  cursor: pointer;
  border-radius: 3px;
}
.options-menu-trigger:hover {
  background: var(--bg);
}
.options-menu-list {
  position: absolute;
  right: 0;
  top: 100%;
  z-index: 10;
  min-width: 10rem;
  margin-top: 0.25rem;
  background: var(--bg-panel);
  border: 1px solid var(--line);
  border-radius: 3px;
  box-shadow: 0 0.5rem 1.5rem rgba(23, 27, 34, 0.18);
  list-style: none;
  padding: 0.25rem;
}
.options-menu-item {
  display: block;
  width: 100%;
  text-align: left;
  border: none;
  background: transparent;
  padding: 0.45rem 0.6rem;
  font: inherit;
  color: var(--ink);
  cursor: pointer;
  border-radius: 3px;
}
.options-menu-item:hover:not(:disabled) {
  background: var(--bg);
}
.options-menu-item:disabled {
  color: var(--dim);
  cursor: default;
}
```

- [ ] **Step 2: import 추가**

`firecrab-frontend/src/components/Images.tsx`의 `import { ... } from "../api/client";` 블록(현재):

```tsx
import {
  ApiClientError,
  deleteImage,
  deleteVm,
  getBootstrap,
  getImageInstall,
  getImagePackage,
  listImages,
  listVms,
  startBootstrap,
  startImageInstall,
  startImagePackage,
  stopVm,
} from "../api/client";
```

다음으로 교체(알파벳 순서 유지):

```tsx
import {
  ApiClientError,
  cancelBootstrap,
  deleteImage,
  deleteStagedPackage,
  deleteVm,
  getBootstrap,
  getImageInstall,
  getImagePackage,
  listImages,
  listVms,
  startBootstrap,
  startImageInstall,
  startImagePackage,
  stopVm,
} from "../api/client";
```

- [ ] **Step 3: `OptionsMenu` 컴포넌트 추가**

`ImageDetail` 함수 정의(177줄) 바로 위에 추가:

```tsx
/**
 * 클릭하면 열리는 최소 드롭다운 메뉴. 바깥 클릭 또는 Esc로 닫힌다.
 * 이 프로젝트에 다른 드롭다운 패턴이 없어 이 자리 전용으로 최소 구현했다 —
 * 범용화해서 다른 화면에 재사용할 계획은 없다.
 */
function OptionsMenu({
  items,
}: {
  items: { label: string; onClick: () => void; disabled: boolean }[];
}) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDocClick = (event: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(event.target as Node)) setOpen(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDocClick);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("mousedown", onDocClick);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [open]);

  return (
    <div className="options-menu" ref={rootRef}>
      <button type="button" className="options-menu-trigger" onClick={() => setOpen((current) => !current)}>
        ⋯
      </button>
      {open && (
        <ul className="options-menu-list">
          {items.map((item) => (
            <li key={item.label}>
              <button
                type="button"
                className="options-menu-item"
                disabled={item.disabled}
                onClick={() => {
                  setOpen(false);
                  item.onClick();
                }}
              >
                {item.label}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
```

- [ ] **Step 4: `ImageDetail` 전체 교체**

`function ImageDetail({ ... }) { ... }` 함수 전체(현재 177-371줄, 시그니처부터 마지막 닫는 `}`까지)를 아래로 통째로 교체한다:

```tsx
function ImageDetail({
  image,
  usedByVms,
  usedByError,
  packageJob,
  busyAlias,
  install,
  onInstallStaged,
  onFetchPackage,
  onDelete,
  bootstrapSession,
  bootstrapStartingAlias,
  bootstrapError,
  onStartBootstrap,
  onCancelBootstrap,
  onDeleteStagedPackage,
}: {
  image: ImageResponse;
  usedByVms: VmResponse[] | null;
  usedByError: string | null;
  packageJob: ImageInstallResponse | undefined;
  busyAlias: string | null;
  install: ImageInstallResponse | null;
  onInstallStaged: (alias: string) => Promise<void>;
  onFetchPackage: (alias: string) => Promise<void>;
  onDelete: (alias: string) => Promise<void>;
  bootstrapSession: BootstrapResponse | null;
  bootstrapStartingAlias: string | null;
  bootstrapError: string | null;
  onStartBootstrap: (alias: string) => Promise<void>;
  onCancelBootstrap: (bootstrapId: string) => Promise<void>;
  onDeleteStagedPackage: (alias: string) => Promise<void>;
}) {
  const fetching = packageJob?.status === "running";
  // Bootstrapping this alias would spend ~30 minutes producing a package
  // the install step then refuses (`already_installed`) or that is already
  // sitting on disk waiting to be installed.
  const blockedByStatus = image.installed || image.packageStaged;
  const bootstrapBusy =
    bootstrapStartingAlias !== null ||
    (bootstrapSession !== null && bootstrapSession.status !== "succeeded" && bootstrapSession.status !== "failed");
  const bootstrapIsMine =
    bootstrapStartingAlias === image.alias || bootstrapSession?.alias === image.alias;

  // "구울 필요 없음"은 이 태스크 이전부터 있던 문구가 아니라 이 플랜이
  // 의도적으로 바꾸는 것이다 — 옛 라벨("이미 설치됨/패키지 준비됨")이
  // 옆의 설치 버튼 라벨("설치됨")과 단어가 겹쳐 사용자가 두 버튼의
  // 차이를 헷갈려 한다는 피드백으로 바뀌었다. 리뷰에서 "기존 버튼과
  // 텍스트가 다르다"는 지적이 나올 수 있는데, 이 한 줄은 그 지적의
  // 예외로 이미 확정된 변경이다.
  const bakeLabel = blockedByStatus
    ? "구울 필요 없음"
    : bootstrapIsMine && bootstrapBusy
      ? "굽는 중…"
      : bootstrapBusy
        ? "다른 배포판 굽는 중"
        : "굽기";

  const installLabel = image.installed
    ? "설치됨"
    : image.packageStaged
      ? busyAlias === image.alias
        ? "설치 중…"
        : "로컬 패키지 설치"
      : image.packageUrl
        ? fetching
          ? "가져오는 중…"
          : `가져오기 (${packageBasename(image.packageUrl)})`
        : "패키지 URL 없음";
  const installDisabled = image.installed
    ? true
    : image.packageStaged
      ? busyAlias === image.alias
      : image.packageUrl
        ? fetching || busyAlias === image.alias
        : true;
  const handleInstallClick = () => {
    if (image.packageStaged) void onInstallStaged(image.alias);
    else if (image.packageUrl) void onFetchPackage(image.alias);
  };

  const deleteLabel = busyAlias === image.alias ? "삭제 중…" : "삭제";

  // "굽기삭제"는 상태에 따라 서로 다른 두 동작을 겸한다: 이 alias에 대해
  // 지금 실행 중인 세션 취소, 또는 완료돼 스테이징된 패키지 삭제. 둘은
  // 실제로 배타적이다 — 세션이 `packageStaged`를 참으로 만들 수 있는
  // 시점(성공 종료)엔 이미 `bootstrapBusy`가 검사하는 비종결 상태를
  // 벗어난 뒤다. `bootstrapSession !== null`을 추가로 요구하는 이유:
  // `handleStartBootstrap`이 POST 응답을 기다리는 짧은 구간엔
  // `bootstrapStartingAlias`(→ `bootstrapIsMine`/`bootstrapBusy`)가 먼저
  // 참이 되고 `bootstrapSession`은 아직 null이다 — 이 세션 없는 구간을
  // 빼지 않으면 "부트스트랩 취소"가 활성 상태로 보이지만 취소할 세션이
  // 없어 클릭해도 아무 일도 안 일어나는 창이 생긴다.
  const canCancelBootstrap = bootstrapIsMine && bootstrapBusy && bootstrapSession !== null;
  const canDeleteStagedPackage = image.packageStaged && !canCancelBootstrap;
  const bakeDeleteLabel = canCancelBootstrap ? "부트스트랩 취소" : "구운 패키지 삭제";
  const handleBakeDeleteClick = () => {
    if (canCancelBootstrap && bootstrapSession) {
      if (
        !window.confirm(
          "진행 중인 부트스트랩을 취소할까요?\n빌더 VM을 삭제하며, 지금까지 진행된 내용은 저장되지 않습니다.",
        )
      ) {
        return;
      }
      void onCancelBootstrap(bootstrapSession.bootstrapId);
    } else if (canDeleteStagedPackage) {
      if (!window.confirm(`'${image.alias}' 구운 패키지를 삭제할까요?`)) return;
      void onDeleteStagedPackage(image.alias);
    }
  };

  return (
    <div className="subpanel">
      <div className="subpanel-header">
        <OptionsMenu
          items={[
            {
              label: bakeLabel,
              disabled: blockedByStatus || bootstrapBusy,
              onClick: () => void onStartBootstrap(image.alias),
            },
            {
              label: installLabel,
              disabled: installDisabled,
              onClick: handleInstallClick,
            },
            {
              label: deleteLabel,
              disabled: !image.installed || busyAlias === image.alias,
              onClick: () => void onDelete(image.alias),
            },
            {
              label: bakeDeleteLabel,
              disabled: !canCancelBootstrap && !canDeleteStagedPackage,
              onClick: handleBakeDeleteClick,
            },
          ]}
        />
      </div>

      <dl className="detail-fields mono">
        <dt>alias</dt>
        <dd>{image.alias}</dd>

        <dt>버전</dt>
        <dd>{image.version}</dd>

        <dt>최소 디스크</dt>
        <dd>{image.minDiskGb} GiB</dd>

        <dt>rootfs 크기</dt>
        <dd>{formatRootfsSize(image.rootfsSizeBytes)}</dd>

        <dt>상태</dt>
        <dd>
          {image.installed
            ? "설치됨"
            : packageJob?.status === "succeeded" || image.packageStaged
              ? "패키지 준비됨"
              : "미설치"}
        </dd>

        <dt>설명</dt>
        <dd>{image.description || "—"}</dd>

        {image.packageUrl && (
          <>
            <dt>패키지 URL</dt>
            <dd>{image.packageUrl}</dd>
          </>
        )}

        <dt>사용 중인 VM</dt>
        <dd>
          {usedByError
            ? usedByError
            : usedByVms === null
              ? "불러오는 중…"
              : usedByVms.length === 0
                ? "없음"
                : usedByVms.map((vm) => `${vm.name} [${vm.state}]`).join(", ")}
        </dd>
      </dl>

      {/* `bootstrapIsMine`으로 걸지 않는다: `startBootstrap` POST 자체가
          실패하면 이 alias의 세션이 아예 생기지 않아 `bootstrapIsMine`이
          항상 거짓이 된다. `ImageDetail`은 한 번에 하나의 alias에만
          렌더링되고, `Images()`의 `selectedAlias`-change effect가 선택이
          바뀔 때마다 `bootstrapError`를 리셋하므로 이 상태는 항상
          "지금 열려있는 alias의 에러"다. */}
      {bootstrapError && <div className="field-error">{bootstrapError}</div>}

      {bootstrapIsMine && bootstrapSession && (
        <>
          <div className="state-badge">{bootstrapSession.status}</div>
          <BootstrapStepper timeline={bootstrapSession.stepTimeline} />
          {/* The builder VM only exists while the session is pre-terminal:
              `packaging` is entered *after* stop_vm returns, and the VM is
              deleted at the end. Gate on status, never on vmId — that field
              keeps its value after the VM it names is gone. */}
          {bootstrapSession.status === "booting" || bootstrapSession.status === "running" ? (
            <InlineConsole vmId={bootstrapSession.vmId} />
          ) : (
            <p className="inline-console-ended">빌더 VM이 정리되어 콘솔 연결이 종료되었습니다.</p>
          )}
          <pre className="detail-log">{bootstrapSession.log}</pre>
        </>
      )}

      {install && install.alias === image.alias && install.status !== "idle" && (
        <>
          <div className="log-export-bar">
            <span className="log-export-bar-label">이미지 가져오기 로그 — {install.alias}</span>
            <LogExportActions
              text={install.log}
              filename={logDownloadFilename("m2image-import", install.alias)}
              buttonClassName="btn console-bar-btn"
              disabled={!install.log}
            />
          </div>
          <pre className="detail-log image-install-log">{install.log}</pre>
        </>
      )}
    </div>
  );
}
```

- [ ] **Step 5: `Images()`에 새 핸들러 2개 추가**

`handleDelete` 함수(611-645줄 부근)의 닫는 `};` 바로 다음에 추가:

```tsx
  const handleDeleteStagedPackage = async (alias: string) => {
    setBusyAlias(alias);
    setActionError(null);
    try {
      await deleteStagedPackage(alias);
      await refreshList();
    } catch (error) {
      setActionError((error as Error).message);
    } finally {
      setBusyAlias(null);
    }
  };

  // 취소 실패는 부트스트랩 자체의 진행 실패가 아니라 사용자가 시작한 별도
  // 액션이므로, 세션 전용 `bootstrapError`가 아니라 일반 `actionError`
  // 배너에 표시한다. 성공 시 세션을 즉시 지운다 — 다음 폴링 틱을 기다리면
  // (`pollBootstrap`의 404 처리가 결국 같은 일을 하긴 하지만) 최대 1초
  // 동안 이미 취소된 세션이 화면에 남는다.
  const handleCancelBootstrap = async (bootstrapId: string) => {
    setActionError(null);
    try {
      await cancelBootstrap(bootstrapId);
      setBootstrapSession(null);
    } catch (error) {
      setActionError((error as Error).message);
    }
  };
```

- [ ] **Step 6: 호출부에 새 prop 2개 배선**

`<ImageDetail ... onStartBootstrap={handleStartBootstrap} />` 호출(696-711줄 부근)에서:

```tsx
            onStartBootstrap={handleStartBootstrap}
          />
```

다음으로 교체:

```tsx
            onStartBootstrap={handleStartBootstrap}
            onCancelBootstrap={handleCancelBootstrap}
            onDeleteStagedPackage={handleDeleteStagedPackage}
          />
```

- [ ] **Step 7: 빌드 + 린트 확인**

Run: `cd firecrab-frontend && npm run build`
Expected: 에러 없음(특히 `noUnusedLocals`/`noUnusedParameters` 관련 에러가 없어야 한다 — 나면 Step 2~6 중 빠뜨린 배선이 있는지 확인).

Run: `npm run lint`
Expected: 에러 0건.

- [ ] **Step 8: 브라우저 수동 확인**

Run: `cd firecrab-frontend && npm run dev`(API 서버도 별도로 실행 중이어야 함 — `docs/20-guides/web.md` 참고)

1. 미설치 alias 상세 클릭 → 우상단 "⋯" 클릭 → 4항목(굽기/설치/삭제/구운 패키지 삭제) 다 보이는지, 굽기만 활성인지 확인
2. 굽기 클릭 → 진행 중 "⋯" 다시 열기 → 4번째 항목 라벨이 "부트스트랩 취소"로 바뀌었는지 → 클릭 → 확인창 → 취소되고 세션이 사라지는지
3. 굽기 완료(스테이징됨) 후 "⋯" → "구운 패키지 삭제" 활성인지 → 클릭 → 확인창 → 삭제 후 표 상태가 "가져오기"로 돌아오는지(재부트스트랩 전엔 다시 못 구움 — 정상)
4. 메뉴 연 상태에서 바깥 아무 데나 클릭 → 닫히는지, 다시 열고 Esc → 닫히는지
5. 이미 설치된 alias에서 "설치" 항목이 "설치됨"(비활성), "삭제"가 활성으로 나오는지(기존 동작 회귀 확인)

- [ ] **Step 9: 커밋**

worktree 안이면:
```bash
git add firecrab-frontend/src/index.css firecrab-frontend/src/components/Images.tsx
git commit -m "feat(frontend): consolidate image actions into an options menu"
```
메인 브랜치에서 직접 실행 중이면 스테이징 후 제안만 한다.

---

### Task 4: 전체 수동 검증 + 정리

**Files:**
- Modify: 없음(검증 태스크. Step 1에서 정리할 게 나오면 `firecrab-frontend/src/components/Images.tsx`)

**Interfaces:**
- Consumes: Task 1~3에서 만들어진 최종 백엔드/프론트 전체.
- Produces: 없음.

- [ ] **Step 1: 죽은 코드 스캔**

```bash
grep -n "package-row" firecrab-frontend/src/components/Images.tsx
```
Expected: 0건(이전 3버튼 블록이 완전히 사라졌어야 한다 — `VmDetailModal.tsx` 등 다른 파일의 `package-row` 사용은 이번 스코프 밖이라 안 건드린다).

```bash
grep -n "onInstallStaged={handleInstallStaged}\|onFetchPackage={handleFetchPackage}\|onDelete={handleDelete}\|onStartBootstrap={handleStartBootstrap}\|onCancelBootstrap={handleCancelBootstrap}\|onDeleteStagedPackage={handleDeleteStagedPackage}" firecrab-frontend/src/components/Images.tsx
```
Expected: 6건 모두 한 번씩, 같은 `<ImageDetail>` 호출 안에.

- [ ] **Step 2: 최종 빌드 + 린트 + 테스트**

Run: `cargo test -p firecrab-api delete_staged_package`
Expected: 4개 통과.

Run: `cd firecrab-frontend && npm run build && npm run lint`
Expected: 둘 다 에러 없음.

- [ ] **Step 3: 스펙의 5개 수동 시나리오 재확인**

Task 3 Step 8에서 이미 실행했다면 결과만 다시 확인한다. 아직이라면 지금 실행 — 5개 시나리오는 Task 3 Step 8과 동일(`docs/superpowers/specs/2026-08-06-m2image-options-menu-design.md`의 "테스트 전략" 절과 일치).

- [ ] **Step 4: 커밋**

Step 1에서 정리할 게 있었을 때만:
```bash
git add firecrab-frontend/src/components/Images.tsx
git commit -m "chore(frontend): drop dead code left over from the options-menu refactor"
```
정리할 게 없었으면 이 스텝은 생략(빈 커밋 없음). 메인 브랜치에서 직접 실행 중이면 커밋 대신 스테이징 후 제안만 한다.

## 완료 기준

- [ ] `DELETE /api/images/{alias}/package`가 스테이징된 패키지를 지우고, 진행 중 작업/미스테이징/미지 alias에 각각 409/409/404로 응답한다
- [ ] 상세 패널에 3버튼 대신 "⋯" 메뉴 하나, 클릭하면 4항목(굽기/설치/삭제/굽기삭제)이 펼쳐진다
- [ ] 굽기/설치/삭제 3항목의 라벨·비활성 규칙이 이전 버튼과 완전히 동일하다(회귀 없음)
- [ ] "굽기삭제"가 진행 중 세션엔 취소, 완료된 스테이징 패키지엔 삭제로 동작하고 각각 확인창을 띄운다
- [ ] 메뉴가 바깥 클릭·Esc로 닫힌다
- [ ] `cargo test -p firecrab-api`(신규 4개), `npm run build`, `npm run lint` 모두 통과
- [ ] 5개 수동 시나리오 통과
