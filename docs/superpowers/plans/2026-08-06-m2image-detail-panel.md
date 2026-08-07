# M2Image 상세 패널 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Images 화면의 표에서 액션 열을 없애고, 행을 클릭하면 열리는 인라인 상세 패널 안에 기본 정보·사용 중인 VM·굽기/설치/삭제 3개 액션을 모은다. 지금 페이지 하단에 항상 떠 있는 "배포판 부트스트랩" 패널은 사라지고 그 기능(부트스트랩 세션 상태·스텝퍼·라이브 콘솔·로그)이 상세 패널 안으로 옮겨온다.

**Architecture:** `firecrab-frontend/src/components/Images.tsx` 한 파일만 수정한다. `BootstrapPanel` 컴포넌트를 없애고 그 상태(`session`/`starting`/폴링 로직)를 `Images()`로 끌어올린다. 표 아래에는 선택된 alias 하나에 대해서만 렌더링되는 새 `ImageDetail` 서브컴포넌트를 두고, `Images()`가 들고 있는 상태(설치 진행 상태·부트스트랩 세션·사용 중 VM 목록)를 props로 내려준다. 백엔드 API/wire 타입은 전혀 바뀌지 않는다.

**Tech Stack:** React 19 + TypeScript(strict, `noUnusedLocals`/`noUnusedParameters` 켜짐), Vite. 테스트 프레임워크는 프로젝트에 설치돼 있지 않다 — 검증은 `npm run build`(`tsc -b` 타입체크 포함)와 `npm run lint`(oxlint), 그리고 브라우저 수동 확인으로 한다(이 컴포넌트 파일 전체가 기존에도 이 방식으로만 검증돼 왔다).

## Global Constraints

- 백엔드 API·wire 타입(`firecrab-api`, `firecrab-api-types`, `bindings/*.ts`) 변경 금지 — 기존 `install`/`package`/`bootstrap` 엔드포인트만 재사용한다.
- `firecrab-frontend/src/components/Images.tsx` 한 파일만 수정한다.
- 대상 alias는 지금과 동일한 3개(alpine-3.24/ubuntu-26.04/rocky-9)로 고정 — 목록 확장 로직은 추가하지 않는다.
- 새 CSS 규칙을 추가하지 않는다 — `is-selected`, `subpanel`, `detail-fields`, `package-row`, `state-badge`, `detail-log`, `inline-console-ended`, `log-export-bar` 등 필요한 클래스가 `firecrab-frontend/src/index.css`에 이미 있다(`table.image-table tbody tr.is-selected`가 이미 정의돼 있음, 지금까지 쓰이지 않았을 뿐).
- 각 태스크가 끝난 시점에 `npm run build`(빌드 디렉터리: `firecrab-frontend`)와 `npm run lint`가 반드시 통과해야 한다.
- 매 태스크 끝에 실제로 `git commit`한다. 이 작업은 이 플랜 전용으로 격리된 git worktree(별도 브랜치)에서 진행되며, 태스크 리뷰·수정 루프가 커밋 diff를 비교하는 데 커밋이 반드시 필요하다 — "에이전트가 직접 커밋하지 않는다"는 방침은 사용자의 실제 작업 브랜치에 적용되는 것이지 이 격리된 워크트리에는 적용되지 않는다. 이 브랜치를 사용자 브랜치에 실제로 병합할지는 모든 태스크와 최종 리뷰가 끝난 뒤 `finishing-a-development-branch` 단계에서 사용자가 결정한다.

---

## 사전 지식 — 지금 파일의 구조(수정 전)

`firecrab-frontend/src/components/Images.tsx`는 지금 다음 3개 모듈 레벨 요소로 구성된다:

1. 순수 헬퍼: `KNOWN_TEMPLATES`, `packageBasename`, `formatRootfsSize`, `keepNewestJobSnapshot`, `BOOTSTRAP_STEPS`, `BOOTSTRAP_STEP_LABEL`, `stepDetailPreview`, `formatElapsed` — **이번 작업에서 전혀 건드리지 않는다.**
2. `BootstrapStepper({ timeline })` — 4박스 스테퍼 렌더 컴포넌트. **그대로 재사용**(정의 안 건드림, 호출 위치만 이동).
3. `BootstrapPanel({ onFinished, unavailableAliases })` — 지금 페이지 하단에 항상 떠 있는 독립 `<section>`. `session`/`error`/`starting` 상태, `pollBootstrap`/`start` 함수, 자체 `mountedRef`를 갖고 있다. **Task 4에서 이 컴포넌트 정의를 완전히 삭제하고 로직을 `Images()`로 옮긴다.**
4. `export default function Images()` — 표 + 액션 열 + 설치 진행 로그 + `<BootstrapPanel>` 호출.

`ImageResponse` 타입(`../bindings`)의 필드: `alias, version, kernelSha256, rootfsSha256, initrdSha256?, minDiskGb, rootfsSizeBytes?, installed, packageUrl?, packageStaged?, description`.

`VmResponse` 타입의 관련 필드: `id, name, state, template`(용도: `vm.template === alias` 필터).

`BootstrapResponse` 타입의 관련 필드: `bootstrapId, alias, vmId, status, log, stepTimeline`(타입은 `BootstrapStepRun[]`).

---

### Task 1: 행 클릭 선택 + 최소 상세 패널 추가 (표 구조는 아직 그대로 둠)

**중요 — 왜 액션 열을 아직 지우지 않는가:** `firecrab-frontend`는 `tsconfig.app.json`에 `noUnusedLocals`/`noUnusedParameters`가 켜져 있다. 표의 액션 열(`<td className="actions">`)을 지우면 그 안에서만 쓰이던 `busyAlias`/`handleInstallStaged`/`handleFetchPackage`/`handleDelete`/`packageBasename`/`fetching`이 그 순간 전부 미사용 상태가 되어 **빌드가 깨진다** — 이 함수들의 새 호출처(`ImageDetail`)는 Task 3에서야 생긴다. 그래서 액션 열 제거와 `ImageDetail`로의 이전은 Task 3에서 **한 스텝 안에 동시에** 한다(제거와 재사용이 같은 커밋에 있어야 미사용 구간이 아예 생기지 않는다). 이번 Task 1은 표 구조를 전혀 건드리지 않고 순수 추가만 한다.

**Files:**
- Modify: `firecrab-frontend/src/components/Images.tsx`

**Interfaces:**
- Produces: `Images()` 안에 `const [selectedAlias, setSelectedAlias] = useState<string | null>(null);`. 이후 모든 태스크가 이 state를 그대로 재사용한다(이름 바뀌지 않음).
- Produces: 새 컴포넌트 `function ImageDetail({ image }: { image: ImageResponse })` — 이후 태스크들이 여기에 prop을 추가해 나간다(컴포넌트 이름 `ImageDetail` 고정, 이후 태스크에서 참조).
- 알려진 임시 상태(버그 아님, 리뷰에서 지적할 필요 없음): 이 태스크가 끝난 시점엔 표 행에 `onClick`(선택 토글)이 걸리면서 그 안의 기존 삭제/설치 버튼도 함께 살아있다 — 그 버튼을 클릭하면 클릭 이벤트가 행까지 버블링되어 선택도 같이 토글된다. 액션 버튼 자체가 Task 3에서 행 밖(상세 패널)으로 옮겨지면서 이 상태는 사라진다. `stopPropagation`을 추가하는 등의 대응은 하지 않는다 — 어차피 한 태스크 뒤에 지워질 코드에 들이는 노력이다.

- [ ] **Step 1: 표 본문 행에 선택 토글(`onClick`)과 `is-selected` 클래스 추가 (그 외 행 내용은 전혀 바꾸지 않음)**

`firecrab-frontend/src/components/Images.tsx`에서 (현재 526줄 부근) `<tr key={image.alias}>`를:

```tsx
                <tr key={image.alias}>
```

다음으로 교체한다(같은 줄, 딱 이 부분만):

```tsx
                <tr
                  key={image.alias}
                  className={selectedAlias === image.alias ? "is-selected" : undefined}
                  onClick={() => setSelectedAlias(selectedAlias === image.alias ? null : image.alias)}
                >
```

행 안의 `<td>` 4개(이미지/크기/상태/액션)는 이 스텝에서 한 글자도 바꾸지 않는다 — `job`/`fetching`/`busyAlias`/`handleDelete` 등 기존 로직 전부 그대로 남는다.

- [ ] **Step 2: `selectedAlias` state와 파생 `selectedImage` 추가**

`Images()` 함수 본문 맨 위, `const [install, setInstall] = useState<ImageInstallResponse | null>(null);` 바로 아래(현재 330줄 부근)에 추가:

```tsx
  const [selectedAlias, setSelectedAlias] = useState<string | null>(null);
```

`refreshList`/`useEffect` 다음, `bootstrapBlockedAliases` 정의 바로 위에 추가(파생값이므로 `useMemo` 불필요 — `images`/`selectedAlias`가 바뀔 때마다 다시 계산해도 배열 순회 한 번뿐):

```tsx
  const selectedImage = (images ?? []).find((image) => image.alias === selectedAlias) ?? null;
```

- [ ] **Step 3: `ImageDetail` 최소 버전 추가 + 렌더링**

`Images()` 함수 정의 바로 위(즉 `BootstrapPanel` 함수 정의와 `export default function Images()` 사이)에 새 컴포넌트를 추가한다:

```tsx
/**
 * 선택된 이미지 하나의 상세 정보 + 액션. 표 아래 인라인으로 열리며,
 * MicroNetworks/MicroStorages의 행 클릭 → 상세 패턴과 동일하다.
 */
function ImageDetail({ image }: { image: ImageResponse }) {
  return (
    <div className="subpanel">
      <dl className="detail-fields mono">
        <dt>버전</dt>
        <dd>{image.version}</dd>

        <dt>최소 디스크</dt>
        <dd>{image.minDiskGb} GiB</dd>

        <dt>rootfs 크기</dt>
        <dd>{formatRootfsSize(image.rootfsSizeBytes)}</dd>

        <dt>설명</dt>
        <dd>{image.description || "—"}</dd>
      </dl>
    </div>
  );
}
```

표를 감싼 `</table>` 바로 다음, 기존 `{install && install.status !== "idle" && (...)}` 블록 앞에 추가한다(현재 578-579줄 부근 — 이 install 블록은 Task 3까지는 그대로 둔다):

```tsx
        </table>
        {selectedImage && <ImageDetail image={selectedImage} />}
        {install && install.status !== "idle" && (
```

- [ ] **Step 4: 빌드 + 린트 확인**

Run: `cd firecrab-frontend && npm run build`
Expected: `tsc -b`와 `vite build`가 에러 없이 끝난다. 이 태스크는 순수 추가만 했으므로(기존 코드를 하나도 지우지 않음) unused-local 에러가 날 이유가 없다 — 만약 난다면 Step 1~3에서 뭔가를 실수로 지운 것이니 원래 내용과 diff를 대조한다.

Run: `cd firecrab-frontend && npm run lint`
Expected: 에러 0건.

- [ ] **Step 5: 브라우저 수동 확인**

Run: `cd firecrab-frontend && npm run dev` (별도 터미널에서 API 서버도 실행 중이어야 함 — `docs/20-guides/web.md` 참고)

브라우저에서 Images 탭 진입 → 이미지 행 하나를 클릭 → 표 아래에 버전/최소 디스크/rootfs 크기/설명이 담긴 박스가 열리는지 확인 → 같은 행을 다시 클릭 → 박스가 닫히는지 확인 → 다른 행 클릭 → 박스 내용이 그 행 것으로 바뀌는지 확인. 액션 버튼(설치/삭제/굽기)이 이미 상세 패널이 아닌 행 안에 남아있고, 그걸 클릭하면 선택도 같이 토글되는 것은 이 태스크에서는 정상이다(Task 3에서 행 밖으로 옮겨지며 사라짐 — 위 Interfaces의 "알려진 임시 상태" 참고).

- [ ] **Step 6: 커밋**

```bash
cd firecrab-frontend && npm run build && npm run lint
git add firecrab-frontend/src/components/Images.tsx
git commit -m "refactor(frontend): make the image table row-selectable"
```

이 저장소의 "직접 커밋하지 않는다" 방침은 사용자의 실제 작업 브랜치에 적용되는 것이다 — 지금은 이 플랜 전용으로 격리된 git worktree(`sdd/m2image-detail-panel` 브랜치)에서 작업 중이며, 여기서의 커밋은 태스크 리뷰/수정 루프가 diff를 비교하는 데 반드시 필요하다. 이 브랜치를 실제로 병합할지는 나중에 `finishing-a-development-branch` 단계에서 사용자가 결정한다 — 지금은 정상적으로 커밋한다.

---

### Task 2: 상세 패널에 "이 이미지를 쓰는 VM" 목록 추가

**Files:**
- Modify: `firecrab-frontend/src/components/Images.tsx`

**Interfaces:**
- Consumes: Task 1의 `selectedAlias`(state), `ImageDetail`(컴포넌트), `image: ImageResponse` prop.
- Produces: `Images()`에 `usedByVms: VmResponse[] | null`, `usedByError: string | null` state. `ImageDetail`에 `usedByVms`/`usedByError` prop 추가(둘 다 타입 그대로 전달) — 이후 태스크가 그대로 재사용.

- [ ] **Step 1: `Images()`에 사용 중 VM 조회 상태 + effect 추가**

`const [selectedImage, ...]` 계산 바로 아래에 추가:

```tsx
  const [usedByVms, setUsedByVms] = useState<VmResponse[] | null>(null);
  const [usedByError, setUsedByError] = useState<string | null>(null);

  // MicroNetworks의 `getMicroNetwork(selectedId)`와 같은 패턴 —
  // 목록 자체엔 없는, 선택 시점의 최신 사용처만 별도로 가져온다.
  useEffect(() => {
    if (!selectedAlias) {
      setUsedByVms(null);
      setUsedByError(null);
      return;
    }
    setUsedByVms(null);
    setUsedByError(null);
    listVms()
      .then((vms) => setUsedByVms(vms.filter((vm) => vm.template === selectedAlias)))
      .catch((error) => setUsedByError((error as Error).message));
  }, [selectedAlias]);
```

- [ ] **Step 2: `ImageDetail`에 prop 추가 + `<dl>`에 항목 추가**

```tsx
function ImageDetail({
  image,
  usedByVms,
  usedByError,
}: {
  image: ImageResponse;
  usedByVms: VmResponse[] | null;
  usedByError: string | null;
}) {
  return (
    <div className="subpanel">
      <dl className="detail-fields mono">
        <dt>버전</dt>
        <dd>{image.version}</dd>

        <dt>최소 디스크</dt>
        <dd>{image.minDiskGb} GiB</dd>

        <dt>rootfs 크기</dt>
        <dd>{formatRootfsSize(image.rootfsSizeBytes)}</dd>

        <dt>설명</dt>
        <dd>{image.description || "—"}</dd>

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
    </div>
  );
}
```

- [ ] **Step 3: 호출부에 새 prop 배선**

```tsx
        {selectedImage && (
          <ImageDetail image={selectedImage} usedByVms={usedByVms} usedByError={usedByError} />
        )}
```

- [ ] **Step 4: 빌드 + 린트 확인**

Run: `cd firecrab-frontend && npm run build`
Expected: 에러 없음.

Run: `cd firecrab-frontend && npm run lint`
Expected: 에러 0건.

- [ ] **Step 5: 브라우저 수동 확인**

이미 설치된 템플릿으로 VM을 하나 생성 → Images 탭에서 그 템플릿 이미지 행 클릭 → "사용 중인 VM"에 방금 만든 VM이 `이름 [상태]` 형식으로 나오는지 확인. VM이 없는 이미지를 클릭했을 땐 "없음"이 나오는지 확인.

- [ ] **Step 6: 커밋**

```bash
cd firecrab-frontend && npm run build && npm run lint
git add firecrab-frontend/src/components/Images.tsx
git commit -m "feat(frontend): list the VMs using an image in its detail panel"
```

이 작업은 이 플랜 전용으로 격리된 git worktree(별도 브랜치)에서 진행 중이다 — "에이전트가 직접 커밋하지 않는다"는 방침은 사용자의 실제 작업 브랜치에 적용되는 것이지 이 워크트리에는 적용되지 않는다. 여기서의 커밋은 태스크 리뷰 루프가 diff를 비교하는 데 필요하므로 정상적으로 커밋한다.

---

### Task 3: 설치·삭제 액션을 상세 패널로 이동, 표에서 액션 열 제거

**이 태스크가 표의 액션 열 제거까지 함께 하는 이유:** Task 1은 표 구조를 건드리지 않고 행 선택만 추가했다 — 액션 열(`<td className="actions">`, 그 안의 `busyAlias`/`handleInstallStaged`/`handleFetchPackage`/`handleDelete`/`packageBasename`/`fetching` 사용)이 아직 그대로 남아있다. 이번 태스크는 그 액션 열을 지우는 것과 동시에(Step 2 하나에서) 같은 함수들을 `ImageDetail`의 새 prop으로 다시 연결한다 — 제거와 재사용을 분리하면 그 사이에 `noUnusedLocals` 빌드 에러가 나는 순간이 생긴다.

**Files:**
- Modify: `firecrab-frontend/src/components/Images.tsx`

**Interfaces:**
- Consumes: `Images()`의 기존 `busyAlias`(state), `packageJobs`(state), `install`(state), `handleInstallStaged`/`handleFetchPackage`/`handleDelete`(기존 함수, 시그니처 `(alias: string) => Promise<void>` 그대로 — 이번 태스크에서 정의를 바꾸지 않는다).
- Produces: `ImageDetail`에 `packageJob: ImageInstallResponse | undefined`, `busyAlias: string | null`, `install: ImageInstallResponse | null`, `onInstallStaged: (alias: string) => Promise<void>`, `onFetchPackage: (alias: string) => Promise<void>`, `onDelete: (alias: string) => Promise<void>` prop 추가 — Task 4가 이 중 `busyAlias` prop 옆에 부트스트랩 관련 prop들을 추가한다.
- 순서 주의: Step 1을 적용한 직후(Step 2 적용 전) 파일은 일시적으로 타입 에러 상태다(`ImageDetail` 호출부가 아직 새 필수 prop을 넘기지 않음) — 정상이다. 이 태스크의 모든 스텝을 다 적용한 뒤에만 빌드가 통과하면 된다.

- [ ] **Step 1: `ImageDetail`에 설치/삭제 prop과 액션 UI 추가**

Task 2에서 만든 `ImageDetail` 함수 전체(시그니처+본문)를 아래로 통째로 교체한다:

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
}) {
  const fetching = packageJob?.status === "running";

  return (
    <div className="subpanel">
      <dl className="detail-fields mono">
        <dt>버전</dt>
        <dd>{image.version}</dd>

        <dt>최소 디스크</dt>
        <dd>{image.minDiskGb} GiB</dd>

        <dt>rootfs 크기</dt>
        <dd>{formatRootfsSize(image.rootfsSizeBytes)}</dd>

        <dt>설명</dt>
        <dd>{image.description || "—"}</dd>

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

      <div className="package-row">
        {image.installed ? (
          <button type="button" className="btn" disabled>
            설치됨
          </button>
        ) : image.packageStaged ? (
          <button
            type="button"
            className="btn primary"
            disabled={busyAlias === image.alias}
            onClick={() => void onInstallStaged(image.alias)}
            title="이 호스트에 준비된 로컬 패키지를 바로 설치합니다."
          >
            {busyAlias === image.alias ? "설치 중…" : "로컬 패키지 설치"}
          </button>
        ) : image.packageUrl ? (
          <button
            type="button"
            className="btn primary"
            disabled={fetching || busyAlias === image.alias}
            onClick={() => void onFetchPackage(image.alias)}
            title={image.packageUrl}
          >
            {fetching ? "가져오는 중…" : `가져오기 (${packageBasename(image.packageUrl)})`}
          </button>
        ) : (
          <button type="button" className="btn" disabled>
            패키지 URL 없음
          </button>
        )}

        <button
          type="button"
          className="btn danger"
          disabled={!image.installed || busyAlias === image.alias}
          onClick={() => void onDelete(image.alias)}
        >
          {busyAlias === image.alias ? "삭제 중…" : "삭제"}
        </button>
      </div>

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

- [ ] **Step 2: 표에서 액션 열 제거(Task 1이 그대로 남겨둔 부분)**

표 헤더 (현재 그대로 남아있는 `<th />`까지 포함한 4열):

```tsx
          <thead>
            <tr>
              <th>이미지</th>
              <th>크기</th>
              <th>상태</th>
              <th />
            </tr>
          </thead>
```

마지막 `<th />`를 지워 3열로 만든다:

```tsx
          <thead>
            <tr>
              <th>이미지</th>
              <th>크기</th>
              <th>상태</th>
            </tr>
          </thead>
```

표 본문 행(Task 1이 `<tr>`에 `onClick`/`className`만 추가하고 나머지는 그대로 둔 상태):

```tsx
            {(images ?? []).map((image) => {
              const job = packageJobs[image.alias];
              const fetching = job?.status === "running";
              const statusLabel = image.installed
                ? "설치됨"
                : job?.status === "succeeded" || image.packageStaged
                  ? "패키지 준비됨"
                  : "미설치";
              // Derived/web-built templates won't have a KNOWN_TEMPLATES entry —
              // fall back to plain alias text with no logo for those.
              const known = KNOWN_TEMPLATES.find((template) => template.alias === image.alias);
              return (
                <tr
                  key={image.alias}
                  className={selectedAlias === image.alias ? "is-selected" : undefined}
                  onClick={() => setSelectedAlias(selectedAlias === image.alias ? null : image.alias)}
                >
                  <td className="mono">
                    {known && <img className="image-template-logo" src={known.logoSrc} alt="" />}
                    {image.alias}
                  </td>
                  <td className="mono">{formatRootfsSize(image.rootfsSizeBytes)}</td>
                  <td>
                    <span className={`state-badge${image.installed ? " running" : ""}`}>{statusLabel}</span>
                  </td>
                  <td className="actions">
                    {image.installed ? (
                      <button type="button" className="btn danger" disabled={busyAlias === image.alias} onClick={() => void handleDelete(image.alias)}>
                        {busyAlias === image.alias ? "삭제 중…" : "삭제"}
                      </button>
                    ) : image.packageStaged ? (
                      // Ahead of the packageUrl branch on purpose: when both
                      // are available, a package already on this host wins
                      // over re-downloading the remote one — which would
                      // overwrite a just-bootstrapped local build.
                      <button
                        type="button"
                        className="btn primary"
                        disabled={busyAlias === image.alias}
                        onClick={() => void handleInstallStaged(image.alias)}
                        title="이 호스트에 준비된 로컬 패키지를 바로 설치합니다."
                      >
                        {busyAlias === image.alias ? "설치 중…" : "로컬 패키지 설치"}
                      </button>
                    ) : image.packageUrl ? (
                      <button type="button" className="btn primary" disabled={fetching || busyAlias === image.alias} onClick={() => void handleFetchPackage(image.alias)} title={image.packageUrl}>
                        {fetching ? "가져오는 중…" : `가져오기 (${packageBasename(image.packageUrl)})`}
                      </button>
                    ) : (
                      <span className="poll-note">패키지 URL 없음</span>
                    )}
                  </td>
                </tr>
              );
            })}
```

`fetching`과 `<td className="actions">` 전체를 지운다(`job`은 `statusLabel` 계산에 계속 쓰이므로 남긴다):

```tsx
            {(images ?? []).map((image) => {
              const job = packageJobs[image.alias];
              const statusLabel = image.installed
                ? "설치됨"
                : job?.status === "succeeded" || image.packageStaged
                  ? "패키지 준비됨"
                  : "미설치";
              // Derived/web-built templates won't have a KNOWN_TEMPLATES entry —
              // fall back to plain alias text with no logo for those.
              const known = KNOWN_TEMPLATES.find((template) => template.alias === image.alias);
              return (
                <tr
                  key={image.alias}
                  className={selectedAlias === image.alias ? "is-selected" : undefined}
                  onClick={() => setSelectedAlias(selectedAlias === image.alias ? null : image.alias)}
                >
                  <td className="mono">
                    {known && <img className="image-template-logo" src={known.logoSrc} alt="" />}
                    {image.alias}
                  </td>
                  <td className="mono">{formatRootfsSize(image.rootfsSizeBytes)}</td>
                  <td>
                    <span className={`state-badge${image.installed ? " running" : ""}`}>{statusLabel}</span>
                  </td>
                </tr>
              );
            })}
```

- [ ] **Step 3: 옛 설치 로그 블록 제거 + 호출부에 새 prop 배선**

`</table>` 다음의 기존 블록(Task 1에서 그대로 뒀던 부분, 현재 파일에서는 `{selectedImage && <ImageDetail .../>}` 바로 다음에 위치):

```tsx
        {selectedImage && (
          <ImageDetail image={selectedImage} usedByVms={usedByVms} usedByError={usedByError} />
        )}
        {install && install.status !== "idle" && (
          <>
            <div className="log-export-bar">
              <span className="log-export-bar-label">이미지 가져오기 로그 — {install.alias}</span>
              <LogExportActions text={install.log} filename={logDownloadFilename("m2image-import", install.alias)} buttonClassName="btn console-bar-btn" disabled={!install.log} />
            </div>
            <pre className="detail-log image-install-log">{install.log}</pre>
          </>
        )}
```

다음으로 교체(옛 무조건 블록을 지우고, `ImageDetail` 호출에 새 prop을 추가):

```tsx
        {selectedImage && (
          <ImageDetail
            image={selectedImage}
            usedByVms={usedByVms}
            usedByError={usedByError}
            packageJob={packageJobs[selectedImage.alias]}
            busyAlias={busyAlias}
            install={install}
            onInstallStaged={handleInstallStaged}
            onFetchPackage={handleFetchPackage}
            onDelete={handleDelete}
          />
        )}
```

- [ ] **Step 4: 표 본문에서 이제 안 쓰는 `packageBasename` import/헬퍼 확인**

`packageBasename`은 `ImageDetail` 안에서 계속 쓰이므로 삭제하지 않는다 — 이 스텝은 실수로 지우지 않았는지 확인하는 점검 스텝이다. `grep -n "packageBasename" firecrab-frontend/src/components/Images.tsx`로 여전히 1곳 이상 쓰이는지 확인한다.

- [ ] **Step 5: 빌드 + 린트 확인**

Run: `cd firecrab-frontend && npm run build`
Expected: 에러 없음.

Run: `cd firecrab-frontend && npm run lint`
Expected: 에러 0건.

- [ ] **Step 6: 브라우저 수동 확인**

`FIRECRAB_IMAGE_BASE_URL`이 설정된 환경에서: 미설치 이미지 행 클릭 → "가져오기 (파일명)" 클릭 → 상세 패널 안에 "이미지 가져오기 로그" 블록이 뜨고 진행되는지 확인 → 완료 후 표 상태가 "설치됨"으로 바뀌고 설치 버튼이 "설치됨"(비활성)으로 바뀌는지 확인. 설치된 이미지에서 "삭제" 클릭 → 확인창 → 삭제 후 표에서 상태가 "미설치"로 돌아오는지 확인. VM이 그 이미지를 쓰고 있을 때 삭제를 누르면 기존과 동일하게 "사용 중인 VM ...개가 있습니다" 확인창이 뜨는지도 확인. 표에는 이제 액션 열이 없다(3열만 있어야 한다).

- [ ] **Step 7: 커밋**

```bash
cd firecrab-frontend && npm run build && npm run lint
git add firecrab-frontend/src/components/Images.tsx
git commit -m "refactor(frontend): move install/delete actions into the image detail panel"
```

이 작업은 이 플랜 전용으로 격리된 git worktree(별도 브랜치)에서 진행 중이다 — "에이전트가 직접 커밋하지 않는다"는 방침은 사용자의 실제 작업 브랜치에 적용되는 것이지 이 워크트리에는 적용되지 않는다. 여기서의 커밋은 태스크 리뷰 루프가 diff를 비교하는 데 필요하므로 정상적으로 커밋한다.

---

### Task 4: 굽기(부트스트랩) 액션을 상세 패널로 이동, 독립 패널 제거

**Files:**
- Modify: `firecrab-frontend/src/components/Images.tsx`

**Interfaces:**
- Consumes: 모듈 레벨 `BootstrapStepper`(변경 없음), Task 3까지의 `ImageDetail` props 전부.
- Produces: `Images()`에 `bootstrapSession: BootstrapResponse | null`, `bootstrapError: string | null`, `bootstrapStarting: boolean` state + `handleStartBootstrap: (alias: string) => Promise<void>` 함수. `ImageDetail`에 `bootstrapSession`, `bootstrapStarting`, `bootstrapError`, `onStartBootstrap: (alias: string) => Promise<void>` prop 추가(최종 prop 목록, 이후 태스크 없음).

- [ ] **Step 1: `BootstrapPanel` 컴포넌트 정의 전체 삭제**

`function BootstrapPanel({ onFinished, unavailableAliases }) { ... }` 전체(주석 포함, `BootstrapStepper` 정의와 `ImageDetail`/`Images` 정의 사이에 있는 블록 전체)를 삭제한다. `BootstrapStepper` 함수 정의 자체와 그 위의 `formatElapsed`는 그대로 둔다 — 삭제 대상은 오직 `BootstrapPanel` 함수 하나다.

- [ ] **Step 2: `Images()`에 부트스트랩 상태 + 함수 추가**

`const [install, setInstall] = useState<ImageInstallResponse | null>(null);` 아래, `selectedAlias` 위 또는 아래(순서 무관) 어디든 추가:

```tsx
  const [bootstrapSession, setBootstrapSession] = useState<BootstrapResponse | null>(null);
  const [bootstrapError, setBootstrapError] = useState<string | null>(null);
  /**
   * True from the click itself, not from the response — mirrors
   * `handleInstallStaged` 등의 `busyAlias` 가드와 같은 이유: 응답이
   * 오기 전 더블클릭이 두 번째 POST를 쏴서 빌더 VM이 두 개 뜨는 것을
   * 막는다. 백엔드도 세션 하나만 허용하므로(409) 이중 방어다.
   */
  const [bootstrapStarting, setBootstrapStarting] = useState(false);
```

`pollInstall` 함수 정의 다음(또는 그 근처 아무 곳)에 부트스트랩 폴링/시작 함수를 추가한다 — 옛 `BootstrapPanel.pollBootstrap`/`start`를 그대로 옮기되, 자체 `mountedRef` 대신 `Images()`가 이미 갖고 있는 `mountedRef`를 쓰고, `onFinished` prop 호출 대신 `refreshList()`를 직접 부른다:

```tsx
  // 옛 BootstrapPanel.pollBootstrap과 동일한 폴링 규율 — 404는 취소로
  // 삭제된 세션이라는 확정 신호(그만 폴링), 그 외 에러는 일시적일 수
  // 있으니 계속 폴링한다.
  const pollBootstrap = (bootstrapId: string) => {
    const tick = async () => {
      if (!mountedRef.current) return;
      try {
        const snapshot = await getBootstrap(bootstrapId);
        if (!mountedRef.current) return;
        setBootstrapSession(snapshot);
        if (snapshot.status === "succeeded") {
          await refreshList();
        } else if (snapshot.status !== "failed") {
          setTimeout(() => void tick(), 1000);
        }
        // "failed" is a confirmed terminal state too — stop without retrying.
      } catch (err) {
        if (err instanceof ApiClientError && err.status === 404) {
          if (mountedRef.current) setBootstrapSession(null);
          return;
        }
        if (mountedRef.current) setTimeout(() => void tick(), 1000);
      }
    };
    void tick();
  };

  const handleStartBootstrap = async (alias: string) => {
    if (bootstrapStarting) return;
    setBootstrapStarting(true);
    setBootstrapError(null);
    try {
      const started = await startBootstrap(alias);
      if (!mountedRef.current) return;
      setBootstrapSession(started);
      pollBootstrap(started.bootstrapId);
    } catch (err) {
      if (!mountedRef.current) return;
      setBootstrapError((err as Error).message);
    } finally {
      if (mountedRef.current) setBootstrapStarting(false);
    }
  };
```

**왜 이 스텝이 필요한가:** `handleStartBootstrap`이 `startBootstrap(alias)` POST 자체에서 실패하면(네트워크 오류, 또는 이미 다른 부트스트랩이 진행 중이라 백엔드가 409를 반환하는 경우) `bootstrapSession`은 그대로 `null`이거나 이전 alias의 값에 머문다 — 이 alias의 세션이 아직 한 번도 만들어진 적이 없기 때문이다. Step 4에서 만들 에러 표시를 `bootstrapSession?.alias === image.alias`(`bootstrapIsMine`) 조건에 걸면, 세션이 아예 없는 이 실패 케이스에서 조건이 항상 거짓이 되어 **에러가 상태에는 저장되지만 화면 어디에도 뜨지 않는다** — 사용자는 "굽기"를 눌렀는데 버튼이 잠깐 비활성됐다가 아무 설명 없이 다시 "굽기"로 돌아오는 것만 본다. `ImageDetail`은 한 번에 하나의 alias에 대해서만 렌더링되므로, `bootstrapError`를 세션 유무와 무관하게 "지금 열려있는 alias의 것"으로 취급하면 된다 — 선택이 바뀔 때 이전 alias의 남은 에러를 지우기만 하면 충분하다.

Task 2에서 만든 사용 중 VM 조회 effect(`Images()` 안, `selectedAlias`가 바뀔 때 `usedByVms`/`usedByError`를 리셋하는 부분)를 아래처럼 수정해 `bootstrapError`도 같이 리셋한다:

```tsx
  // MicroNetworks의 `getMicroNetwork(selectedId)`와 같은 패턴 —
  // 목록 자체엔 없는, 선택 시점의 최신 사용처만 별도로 가져온다.
  useEffect(() => {
    if (!selectedAlias) {
      setUsedByVms(null);
      setUsedByError(null);
      return;
    }
    setUsedByVms(null);
    setUsedByError(null);
    listVms()
      .then((vms) => setUsedByVms(vms.filter((vm) => vm.template === selectedAlias)))
      .catch((error) => setUsedByError((error as Error).message));
  }, [selectedAlias]);
```

다음으로 교체(맨 앞에 `setBootstrapError(null);` 한 줄 추가 — 이 effect는 이제 "선택이 바뀔 때 그 이전 alias에 딸려있던 화면 상태를 정리한다"는 책임까지 겸한다):

```tsx
  // MicroNetworks의 `getMicroNetwork(selectedId)`와 같은 패턴 —
  // 목록 자체엔 없는, 선택 시점의 최신 사용처만 별도로 가져온다.
  // `bootstrapError`도 함께 리셋: 세션이 아예 안 생긴 채 실패한 경우
  // `bootstrapSession.alias`로는 "누구 에러인지"를 알 수 없으므로,
  // 대신 선택이 바뀌는 시점에 이전 alias의 에러를 지우는 방식으로
  // 항상 "지금 열려있는 alias의 에러"만 남긴다.
  useEffect(() => {
    setBootstrapError(null);
    if (!selectedAlias) {
      setUsedByVms(null);
      setUsedByError(null);
      return;
    }
    setUsedByVms(null);
    setUsedByError(null);
    listVms()
      .then((vms) => setUsedByVms(vms.filter((vm) => vm.template === selectedAlias)))
      .catch((error) => setUsedByError((error as Error).message));
  }, [selectedAlias]);
```

- [ ] **Step 3: 이제 안 쓰는 `bootstrapBlockedAliases` 제거**

```tsx
  // Bootstrapping either of these would spend ~30 minutes producing a package
  // the install step then refuses (`already_installed`) or that is already
  // sitting on disk waiting to be installed.
  const bootstrapBlockedAliases = useMemo(
    () =>
      (images ?? [])
        .filter((image) => image.installed || image.packageStaged)
        .map((image) => image.alias),
    [images],
  );
```

이 블록 전체를 삭제한다(`ImageDetail` 안에서 `image.installed || image.packageStaged`를 그 자리에서 직접 쓸 것이므로 더는 필요 없다). 이 시점에서 `useMemo`는 이 파일 전체에서 이 한 곳(`bootstrapBlockedAliases`)에만 쓰이고 있었으므로 — `grep -n "useMemo" firecrab-frontend/src/components/Images.tsx`로 지금 이 블록을 지운 뒤 결과가 import 줄 하나만 남는지 확인한다 — 파일 맨 위 import 줄에서도 반드시 함께 지운다:

```tsx
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
```
→
```tsx
import { useCallback, useEffect, useRef, useState } from "react";
```

(`useMemo`가 미사용으로 남으면 Step 6의 `npm run lint`에서 oxlint가 잡아낸다.)

- [ ] **Step 4: `ImageDetail`에 굽기 버튼 + 진행 상황 추가, 호출부 배선**

`ImageDetail`의 props 타입과 본문을 다음으로 교체한다:

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
  bootstrapStarting,
  bootstrapError,
  onStartBootstrap,
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
  bootstrapStarting: boolean;
  bootstrapError: string | null;
  onStartBootstrap: (alias: string) => Promise<void>;
}) {
  const fetching = packageJob?.status === "running";
  const blockedByStatus = image.installed || image.packageStaged;
  const bootstrapBusy =
    bootstrapStarting ||
    (bootstrapSession !== null && bootstrapSession.status !== "succeeded" && bootstrapSession.status !== "failed");
  const bootstrapIsMine = bootstrapSession?.alias === image.alias;

  return (
    <div className="subpanel">
      <dl className="detail-fields mono">
        <dt>버전</dt>
        <dd>{image.version}</dd>

        <dt>최소 디스크</dt>
        <dd>{image.minDiskGb} GiB</dd>

        <dt>rootfs 크기</dt>
        <dd>{formatRootfsSize(image.rootfsSizeBytes)}</dd>

        <dt>설명</dt>
        <dd>{image.description || "—"}</dd>

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

      <div className="package-row">
        <button
          type="button"
          className="btn"
          disabled={blockedByStatus || bootstrapBusy}
          title={
            blockedByStatus
              ? "이미 설치됐거나 설치할 패키지가 이미 준비되어 있습니다."
              : bootstrapBusy && !bootstrapIsMine
                ? "다른 배포판의 부트스트랩이 진행 중입니다."
                : "공식 배포판을 처음부터 준비합니다 — 이미 있는 microVM 빌더를 재사용해 별도 컨테이너나 권한 상승 없이 처리합니다."
          }
          onClick={() => void onStartBootstrap(image.alias)}
        >
          {bootstrapIsMine && bootstrapBusy ? "굽는 중…" : "굽기"}
        </button>

        {image.installed ? (
          <button type="button" className="btn" disabled>
            설치됨
          </button>
        ) : image.packageStaged ? (
          <button
            type="button"
            className="btn primary"
            disabled={busyAlias === image.alias}
            onClick={() => void onInstallStaged(image.alias)}
            title="이 호스트에 준비된 로컬 패키지를 바로 설치합니다."
          >
            {busyAlias === image.alias ? "설치 중…" : "로컬 패키지 설치"}
          </button>
        ) : image.packageUrl ? (
          <button
            type="button"
            className="btn primary"
            disabled={fetching || busyAlias === image.alias}
            onClick={() => void onFetchPackage(image.alias)}
            title={image.packageUrl}
          >
            {fetching ? "가져오는 중…" : `가져오기 (${packageBasename(image.packageUrl)})`}
          </button>
        ) : (
          <button type="button" className="btn" disabled>
            패키지 URL 없음
          </button>
        )}

        <button
          type="button"
          className="btn danger"
          disabled={!image.installed || busyAlias === image.alias}
          onClick={() => void onDelete(image.alias)}
        >
          {busyAlias === image.alias ? "삭제 중…" : "삭제"}
        </button>
      </div>

      {/* `bootstrapIsMine`으로 걸지 않는다: `startBootstrap` POST 자체가
          실패하면 이 alias의 세션이 아예 생기지 않아 `bootstrapIsMine`이
          항상 거짓이 된다(위 Step 2 참고). `ImageDetail`은 한 번에 하나의
          alias에만 렌더링되고, 선택이 바뀔 때마다 `bootstrapError`가
          리셋되므로(Step 2의 effect) 이 상태는 항상 "지금 열려있는
          alias의 에러"다. */}
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

호출부(`Images()`의 return 안, 표 다음)를 다음으로 교체:

```tsx
        {selectedImage && (
          <ImageDetail
            image={selectedImage}
            usedByVms={usedByVms}
            usedByError={usedByError}
            packageJob={packageJobs[selectedImage.alias]}
            busyAlias={busyAlias}
            install={install}
            onInstallStaged={handleInstallStaged}
            onFetchPackage={handleFetchPackage}
            onDelete={handleDelete}
            bootstrapSession={bootstrapSession}
            bootstrapStarting={bootstrapStarting}
            bootstrapError={bootstrapError}
            onStartBootstrap={handleStartBootstrap}
          />
        )}
```

- [ ] **Step 5: `</section>` 다음의 `<BootstrapPanel .../>` 호출 제거**

```tsx
      </section>

      <BootstrapPanel onFinished={() => void refreshList()} unavailableAliases={bootstrapBlockedAliases} />
    </div>
  );
}
```

다음으로 교체:

```tsx
      </section>
    </div>
  );
}
```

- [ ] **Step 6: 빌드 + 린트 확인**

Run: `cd firecrab-frontend && npm run build`
Expected: 에러 없음(특히 `useMemo`/`bootstrapBlockedAliases` 관련 미사용 에러가 없어야 한다).

Run: `cd firecrab-frontend && npm run lint`
Expected: 에러 0건.

- [ ] **Step 7: 브라우저 수동 확인**

미설치 alias(예: rocky-9) 행 클릭 → 상세 안 "굽기" 클릭 → 상태뱃지+스텝퍼+라이브 콘솔(부팅/실행 중)+로그가 상세 패널 안에 뜨는지 확인. 진행 중에 다른 alias 행을 클릭 → 그 alias의 "굽기" 버튼이 "다른 배포판의 부트스트랩이 진행 중입니다" 툴팁과 함께 비활성인지 확인 → 원래 굽던 alias로 돌아가면 진행 상황(스텝퍼/로그)이 그대로 이어지는지 확인. 완료 후 "패키지 준비됨" 상태로 바뀌고 "설치" 버튼이 활성화되는지 확인.

- [ ] **Step 8: 커밋**

```bash
cd firecrab-frontend && npm run build && npm run lint
git add firecrab-frontend/src/components/Images.tsx
git commit -m "refactor(frontend): fold the standalone bootstrap panel into the image detail panel"
```

이 작업은 이 플랜 전용으로 격리된 git worktree(별도 브랜치)에서 진행 중이다 — "에이전트가 직접 커밋하지 않는다"는 방침은 사용자의 실제 작업 브랜치에 적용되는 것이지 이 워크트리에는 적용되지 않는다. 여기서의 커밋은 태스크 리뷰 루프가 diff를 비교하는 데 필요하므로 정상적으로 커밋한다.

---

### Task 5: 전체 수동 검증 + 정리

**Files:**
- Modify: `firecrab-frontend/src/components/Images.tsx` (필요시 정리만, 새 기능 없음)

**Interfaces:**
- Consumes: Task 1~4에서 만들어진 최종 `ImageDetail`/`Images()` 전체.
- Produces: 없음(검증 태스크).

- [ ] **Step 1: 죽은 코드 스캔**

```bash
grep -n "BootstrapPanel\|unavailableAliases\|bootstrapBlockedAliases" firecrab-frontend/src/components/Images.tsx
```
Expected: 아무 결과도 없어야 한다(전부 Task 4에서 제거됨). 뭔가 남아 있으면 지운다.

```bash
grep -rn "BootstrapPanel" firecrab-frontend/src
```
Expected: 다른 파일에서도 참조가 없어야 한다(원래 `Images.tsx` 안에서만 쓰이던 컴포넌트).

- [ ] **Step 2: 최종 빌드 + 린트**

Run: `cd firecrab-frontend && npm run build`
Expected: 에러 없음.

Run: `cd firecrab-frontend && npm run lint`
Expected: 에러 0건.

- [ ] **Step 3: 스펙의 수동 테스트 체크리스트 실행**

`docs/superpowers/specs/2026-08-06-m2image-detail-panel-design.md`의 "테스트 전략" 절에 적힌 5개 시나리오를 alpine-3.24/ubuntu-26.04/rocky-9 3개 alias 각각에서 실행하고 결과를 기록한다:
1. 행 클릭 → 상세 열림 / 재클릭 → 닫힘
2. 미설치 alias 굽기 → 상세 안에서 스텝퍼+콘솔+로그 진행 확인 → 완료 후 설치 버튼 활성화
3. 설치 → 로그 → 완료 후 상태 "설치됨"
4. 삭제 → 사용 중 VM 있는 경우/없는 경우 각각
5. 굽기 진행 중 다른 alias 선택 시 그 alias의 굽기 버튼이 "다른 배포판 굽는 중"으로 비활성

- [ ] **Step 4: 커밋**

Step 1에서 정리할 게 있었다면(빌드+린트가 여전히 깨끗한지 다시 확인 후):

```bash
cd firecrab-frontend && npm run build && npm run lint
git add firecrab-frontend/src/components/Images.tsx
git commit -m "chore(frontend): drop dead code left over from the detail-panel refactor"
```

이 작업은 이 플랜 전용으로 격리된 git worktree(별도 브랜치)에서 진행 중이다 — "에이전트가 직접 커밋하지 않는다"는 방침은 사용자의 실제 작업 브랜치에 적용되는 것이지 이 워크트리에는 적용되지 않는다. 정리할 게 없었다면 이 스텝은 생략한다(빈 커밋 없음).

## 완료 기준

- [ ] Images 표에 액션 열이 없고, 행 클릭으로 상세 패널이 열리고 닫힌다
- [ ] 상세 패널에 버전·최소 디스크·rootfs 크기·설명·사용 중인 VM이 표시된다
- [ ] 상세 패널 안에서 굽기·설치·삭제 3개 액션이 전부 동작한다
- [ ] 부트스트랩 진행 상황(스텝뱃지+스텝퍼+라이브 콘솔+로그)이 상세 패널 안에 뜬다
- [ ] 독립 "배포판 부트스트랩" 패널과 `BootstrapPanel` 컴포넌트가 완전히 제거됐다
- [ ] `npm run build`/`npm run lint` 모두 통과
- [ ] 3개 alias 전체에서 스펙의 5개 수동 테스트 시나리오 통과
