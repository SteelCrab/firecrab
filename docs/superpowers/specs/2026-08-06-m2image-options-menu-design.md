---
tags:
  - firecrab
  - m2image
  - spec
status: 설계 완료
scope: MVP
updated: 2026-08-06
---

# M2Image 상세 패널 — 옵션(⋯) 메뉴 + 구운 패키지 삭제 — 설계

> [!summary] 한 줄 요약
> 상세 패널의 굽기·설치·삭제 3개 버튼을 "⋯" 옵션 메뉴 안 4개 항목(굽기·설치·삭제·굽기삭제)으로
> 재구성하고, 지금까지 지울 방법이 없던 "스테이징만 되고 아직 설치 안 된 로컬 패키지"를 지우는
> 백엔드 엔드포인트를 신설한다.

## 왜

- 상세 패널을 실제로 써 본 사용자가 두 가지를 지적했다: (1) "굽기 삭제 기능도 필요하다" — 부트스트랩으로
  만든 로컬 패키지가 아직 설치 전 상태(`packageStaged && !installed`)일 때 지울 방법이 없다.
  실제로 확인해보니 `DELETE /api/images/{alias}`는 `templates.resolve_alias`로 **설치된** 템플릿만
  대상으로 하고(`firecrab-api/src/handlers/images.rs:293-299`), 스테이징된 패키지만 지우는
  엔드포인트 자체가 없다. (2) "굽기/설치/삭제 3개 버튼을 '...' 옵션 아이콘으로 묶어 4개 기능을
  하게 하자" — 버튼이 늘어날수록 상세 패널이 번잡해지니 메뉴로 정리하자는 요청.
- 이 두 요청은 같은 자리(상세 패널의 액션 영역)를 동시에 바꾸므로 하나의 설계로 묶는다.
- `firecrab-frontend/src/api/client.ts`에 `cancelBootstrap(bootstrapId)`가 이미 있지만 지금까지
  `Images.tsx` 어디에서도 호출되지 않는 죽은 코드였다 — 이번에 "굽기삭제"의 절반(진행 중 세션
  취소)으로 처음 실제 쓰인다.

## 핵심 아이디어

상세 패널 우상단에 "⋯" 버튼 하나만 남기고, 클릭하면 4개 항목(굽기·설치·삭제·굽기삭제)이 드롭다운으로
펼쳐진다. 굽기·설치·삭제 3개는 **지금 코드의 disabled·라벨 로직을 그대로** 메뉴 항목으로 옮기기만
한다(동작 변경 없음). 굽기삭제만 신규 — 상태에 따라 "진행 중인 부트스트랩 취소"(기존
`cancelBootstrap` 재사용) 또는 "완료된 스테이징 패키지 삭제"(신규 백엔드 엔드포인트) 둘 중 하나로
동작한다.

## 의도적 범위 경계

- 이 프로젝트에 드롭다운/옵션 메뉴 UI 패턴이 전혀 없어서(`grep`으로 확인) 이번이 첫 도입이다 —
  기존 컴포넌트가 재사용할 만한 것을 가져다 쓰는 게 아니라 `Images.tsx` 안에 최소 규모로 새로
  만든다. 범용 컴포넌트 라이브러리화(다른 화면에서도 쓸 수 있게 추출)는 하지 않는다 — 지금
  이 자리 하나에만 필요하다.
- 백엔드 변경은 `DELETE /api/images/{alias}/package` 신설 하나뿐이다. 기존
  `install`/`bootstrap`/`bootstrap/{id}` 엔드포인트는 그대로 재사용(신규 로직 없음).
- 다크모드 대응 불필요 — `index.css`에 `prefers-color-scheme`/`data-theme` 자체가 없는 단일 테마
  앱이다(확인 완료).
- 메뉴는 매번 4개 항목을 전부 그리고, 지금 상황에 안 맞는 항목만 회색+비활성으로 표시한다(항목
  자체를 숨기지 않음) — 사용자가 이미 이 방식으로 확정.
- 표 행 클릭으로 상세 패널을 여닫는 기존 동작, `usedByVms` 등 나머지 상세 패널 구조는 전혀
  건드리지 않는다.

## 백엔드 설계

### 새 핸들러: `delete_staged_package`

`firecrab-api/src/handlers/images.rs`에 `delete_image`(같은 파일, 278-345줄) 바로 근처에 추가.
같은 파일의 `get_image_package`(120-147줄)가 이미 쓰는 alias 검증·`staged_package_path`를 그대로
재사용한다:

```rust
/// `DELETE /api/images/{alias}/package` — 스테이징만 되고 아직 설치되지 않은 로컬 패키지
/// (`.packages/{alias}.tar.zst`)를 지운다. 설치된 템플릿 자체를 지우는
/// `delete_image`(별도 엔드포인트)와는 독립적 — 이미 설치된 이미지의 스테이징 패키지가
/// 남아있어도 이 엔드포인트로 지울 수 있다(재설치 시 재다운로드가 필요해짐).
pub async fn delete_staged_package(
    State(state): State<AppState>,
    Path(alias): Path<String>,
    Extension(request_id): Extension<RequestId>,
) -> Result<StatusCode, AppError> {
    // get_image_package와 동일한 alias 검증.
    if TemplateRegistry::known_spec(&alias).is_none()
        && state.templates.resolve_alias(&alias).is_none()
    {
        return Err(AppError::not_found(request_id.0));
    }

    // 다운로드/검증 중이거나 설치 진행 중이면 그 작업이 이 파일을 쓰거나 읽고 있을 수 있다 —
    // delete_image의 install_in_progress 가드와 같은 이유.
    if state.image_packages.is_running(&alias) || state.image_installs.is_running(&alias) {
        return Err(AppError::conflict(
            "package_in_progress",
            "cannot delete while a package download or install is running for this template",
            request_id.0,
        ));
    }

    let image_root = state.templates.image_root_path();
    if !image_install::staged_package_exists(image_root, &alias) {
        return Err(AppError::conflict(
            "not_staged",
            "no staged package exists for this alias",
            request_id.0,
        ));
    }

    let path = image_install::staged_package_path(image_root, &alias);
    tokio::task::spawn_blocking(move || std::fs::remove_file(&path))
        .await
        .map_err(|_| AppError::internal(request_id.0))?
        .map_err(|_| AppError::internal(request_id.0))?;

    Ok(StatusCode::NO_CONTENT)
}
```

`AppError::conflict(code: &'static str, message: &'static str, request_id: Uuid)`(기존 시그니처,
`firecrab-api/src/error.rs:173`) 그대로 사용 — 새 에러 코드 2개(`package_in_progress`,
`not_staged`) 추가되지만 `AppError` 자체의 구조는 안 바뀐다.

### 라우트

`firecrab-api/src/server.rs`의 기존 줄(207번 부근):

```rust
.route(
    "/api/images/{alias}/package",
    get(handlers::images::get_image_package).post(handlers::images::start_image_package),
)
```

다음으로 교체(`.delete(...)` 추가):

```rust
.route(
    "/api/images/{alias}/package",
    get(handlers::images::get_image_package)
        .post(handlers::images::start_image_package)
        .delete(handlers::images::delete_staged_package),
)
```

### 프론트 API 클라이언트

`firecrab-frontend/src/api/client.ts`에 `deleteImage`와 동일한 모양으로 추가:

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

(정확한 형태는 `deleteImage`/`cancelBootstrap`의 현재 구현을 그대로 미러링 — 구현 계획에서 그
둘을 참조한다.)

## 프론트 설계

### `OptionsMenu` 컴포넌트 (신규, `Images.tsx` 안)

```tsx
/**
 * 클릭하면 열리는 최소 드롭다운 메뉴. 바깥 클릭 또는 Esc로 닫힌다.
 * 이 프로젝트에 다른 드롭다운 패턴이 없어 여기 전용으로 최소 구현.
 */
function OptionsMenu({ items }: { items: { label: string; onClick: () => void; disabled: boolean }[] }) {
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

`items`는 `{ label, onClick, disabled }[]` — 각 항목의 라벨/동작/비활성 여부는 `ImageDetail`이
계산해서 넘긴다(아래).

### `ImageDetail` 변경

**새 prop 4개** 추가: `bootstrapStagingBusy`는 안 씀(기존 `bootstrapBusy`/`bootstrapIsMine`
그대로 재사용), 대신 `onCancelBootstrap: (bootstrapId: string) => Promise<void>`와
`onDeleteStagedPackage: (alias: string) => Promise<void>` 2개만 추가.

**렌더 변경**: 현재(217-262줄 부근)

```tsx
  return (
    <div className="subpanel">
      <dl className="detail-fields mono">
```

다음으로(`⋯` 트리거를 담을 헤더 행 추가):

```tsx
  const canCancelBootstrap = bootstrapIsMine && bootstrapBusy;
  const canDeleteStagedPackage = image.packageStaged && !canCancelBootstrap;

  const handleBakeDelete = () => {
    if (canCancelBootstrap && bootstrapSession) {
      if (!window.confirm("진행 중인 부트스트랩을 취소할까요?\n빌더 VM을 삭제하며, 지금까지 진행된 내용은 저장되지 않습니다.")) return;
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
              label: blockedByStatus
                ? "구울 필요 없음"
                : bootstrapIsMine && bootstrapBusy
                  ? "굽는 중…"
                  : bootstrapBusy
                    ? "다른 배포판 굽는 중"
                    : "굽기",
              disabled: blockedByStatus || bootstrapBusy,
              onClick: () => void onStartBootstrap(image.alias),
            },
            {
              label: image.installed
                ? "설치됨"
                : image.packageStaged
                  ? (busyAlias === image.alias ? "설치 중…" : "로컬 패키지 설치")
                  : image.packageUrl
                    ? (fetching ? "가져오는 중…" : `가져오기 (${packageBasename(image.packageUrl)})`)
                    : "패키지 URL 없음",
              disabled:
                image.installed ||
                (image.packageStaged
                  ? busyAlias === image.alias
                  : image.packageUrl
                    ? fetching || busyAlias === image.alias
                    : true),
              onClick: () =>
                image.packageStaged ? void onInstallStaged(image.alias) : void onFetchPackage(image.alias),
            },
            {
              label: busyAlias === image.alias ? "삭제 중…" : "삭제",
              disabled: !image.installed || busyAlias === image.alias,
              onClick: () => void onDelete(image.alias),
            },
            {
              label: canCancelBootstrap ? "부트스트랩 취소" : "구운 패키지 삭제",
              disabled: !canCancelBootstrap && !canDeleteStagedPackage,
              onClick: handleBakeDelete,
            },
          ]}
        />
      </div>
      <dl className="detail-fields mono">
```

기존 `<div className="package-row">...3개 버튼...</div>` 블록 전체는 삭제(내용이 위 메뉴 항목으로
옮겨감).

*(위 코드는 설계 의도를 정확히 보여주기 위한 스케치다 — 구현 계획에서 지금 파일의 정확한
줄 번호 기준 before/after로 다시 정리한다. 특히 `onClick`을 두 갈래 조건으로 다시 계산하는 것보다
`image.packageStaged`/`packageUrl` 분기를 그대로 살린 헬퍼 함수로 뽑는 편이 나을 수 있다 —
계획 작성 시 판단.)*

### 호출부 (`Images()`)

`<ImageDetail ...>` 호출에 두 prop 추가:

```tsx
onCancelBootstrap={cancelBootstrap}
onDeleteStagedPackage={handleDeleteStagedPackage}
```

`cancelBootstrap`은 `api/client.ts`에서 새로 import(기존 함수, 지금까지 미사용).
`handleDeleteStagedPackage`는 `Images()`에 새로 추가하는 핸들러 — `handleDelete`와 같은 모양:

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
```

`cancelBootstrap` 호출 실패 시에도 같은 `actionError` 배너를 쓴다(부트스트랩 전용
`bootstrapError`가 아니라) — 취소는 세션의 진행 실패가 아니라 사용자의 별도 액션이므로 굽기
자체의 에러 상태와 분리한다.

## CSS

새 클래스 4개, 기존 CSS 커스텀 프로퍼티만 재사용(`--bg-panel`, `--line`, `--ink`, `--dim`,
`--error` 등 — `index.css:2-14`):

```css
.subpanel-header {
  display: flex;
  justify-content: flex-end;
}
.options-menu {
  position: relative;
}
.options-menu-trigger {
  /* .btn과 동일한 크기감이되 테두리 없이 — 메뉴 트리거는 본문 버튼보다 가벼워야 한다 */
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

## 트레이드오프

- 4개 항목을 매번 다 그리고 비활성 처리하는 방식은(이미 확정) 메뉴가 항상 4줄 고정 높이라
  일관되지만, 지금 상황에 쓸 수 있는 항목이 1개뿐이어도 나머지 3개가 회색으로 계속 보인다 —
  사용자가 이미 이 트레이드오프를 승인.
- "굽기삭제" 하나로 취소/삭제 두 가지 다른 동작을 겸하는 것은 버튼 개수를 4개로 유지하기 위한
  선택이다 — 두 동작이 상태상 배타적(`bootstrapIsMine && bootstrapBusy`와 `packageStaged`가
  동시에 참일 일이 없음: 세션이 성공적으로 끝나야 `packageStaged`가 참이 되고, 그 시점엔
  `bootstrapBusy`가 이미 거짓)이라 실제로 헷갈릴 상황은 없다.

## 테스트 전략

- 백엔드: `delete_staged_package` 단위테스트 — `delete_image`(handlers/images.rs 기존 테스트들)와
  같은 스타일로: 정상 삭제 후 파일 없음 확인, 미스테이징 상태에서 409, 진행 중 패키지 작업이 있을
  때 409, 알 수 없는 alias에 404.
- 프론트: 기존 관례대로 `npm run build` + `npm run lint`만, 수동 검증은 사람이(브라우저 필요).
- 수동 검증 체크리스트(구현 계획에 포함):
  1. 미설치 alias 상세 → "⋯" → 4항목 다 보이는지, 굽기만 활성인지
  2. 굽기 진행 중 "⋯" 열기 → "부트스트랩 취소" 라벨로 바뀌었는지 → 클릭 → 확인창 → 취소되는지
  3. 굽기 완료(스테이징됨) 후 "⋯" → "구운 패키지 삭제" 활성인지 → 클릭 → 확인창 → 삭제 후
     "가져오기" 상태로 돌아오는지(재부트스트랩 없이는 다시 못 구움 — 정상)
  4. 메뉴 연 상태에서 바깥 클릭 → 닫히는지, Esc → 닫히는지
  5. 기존 굽기/설치/삭제 3개 항목의 disabled·라벨이 이전과 동일하게 동작하는지(회귀 확인)

## 완료 기준 (MVP)

- [ ] `DELETE /api/images/{alias}/package` 신설 + 라우트 등록 + 단위테스트 4종
- [ ] 프론트 `deleteStagedPackage` API 클라이언트 함수
- [ ] `OptionsMenu` 컴포넌트 (바깥 클릭/Esc로 닫힘)
- [ ] `ImageDetail`의 `package-row` 3버튼 → "⋯" 메뉴 4항목으로 교체(굽기/설치/삭제 동작은 기존과
      동일, 라벨/비활성 규칙 그대로 이전)
- [ ] "굽기삭제" 항목: 진행 중 세션엔 취소(`cancelBootstrap` 재사용), 스테이징된 패키지엔 삭제
      (신규 엔드포인트), 확인창 문구 2종
- [ ] 위 5개 시나리오 수동 검증

## 참고

- `firecrab-api/src/handlers/images.rs`의 `delete_image`(278-345줄), `get_image_package`
  (120-147줄) — 새 핸들러가 그대로 미러링하는 기존 패턴.
- `firecrab-api/src/image_install.rs`의 `staged_package_path`/`staged_package_exists`/
  `package_name`, `ImageInstallTracker::is_running` — 새 핸들러가 재사용하는 기존 헬퍼.
- `firecrab-frontend/src/api/client.ts`의 `deleteImage`, `cancelBootstrap`(기존, 지금까지 미사용) —
  새 클라이언트 함수가 미러링/재사용하는 기존 코드.
- `docs/superpowers/specs/2026-08-06-m2image-detail-panel-design.md` — 이번에 확장하는
  `ImageDetail`/상세 패널 자체의 원 설계.
