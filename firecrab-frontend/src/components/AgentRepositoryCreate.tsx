import { useEffect, useRef, useState, type FormEvent } from "react";
import { agentRequest, type GitHubRepository } from "../api/agents";
import { useI18n } from "../i18n";
import AppDialog from "./ui/AppDialog";

export default function AgentRepositoryCreate({
  controllerId,
  vmId,
  canControl,
  onCreated,
  onClose,
  onDirty,
}: {
  controllerId: string;
  vmId: string;
  canControl: boolean;
  onCreated: (repository: GitHubRepository) => Promise<void>;
  onClose: () => void;
  onDirty: (dirty: boolean) => void;
}) {
  const { t } = useI18n();
  const [id, setId] = useState(() => crypto.randomUUID());
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [privateRepository, setPrivateRepository] = useState(true);
  const [initialize, setInitialize] = useState(true);
  const [created, setCreated] = useState<GitHubRepository | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const active = useRef(false);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  useEffect(() => {
    onDirty(
      busy ||
        !!name.trim() ||
        !!description.trim() ||
        !privateRepository ||
        !initialize,
    );
    return () => onDirty(false);
  }, [busy, name, description, privateRepository, initialize, onDirty]);
  const changeRequest = () => {
    setId(crypto.randomUUID());
    setError("");
  };
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!canControl || active.current) return;
    active.current = true;
    setBusy(true);
    setError("");
    try {
      const repository =
        created ||
        (await agentRequest<GitHubRepository>(
          "/github/repositories",
          {
            id,
            name: name.trim(),
            description: description.trim(),
            private: privateRepository,
            initialize,
          },
          controllerId,
          vmId,
        ));
      if (!alive.current) return;
      setCreated(repository);
      await onCreated(repository);
      if (alive.current) onClose();
    } catch (failure) {
      if (alive.current)
        setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      active.current = false;
      if (alive.current) setBusy(false);
    }
  };
  return (
    <AppDialog
      label={t("Create GitHub repository", "GitHub 저장소 만들기")}
      busy={busy}
      onClose={onClose}
    >
      <form
        className="am-repository-create"
        onSubmit={(event) => void submit(event)}
        aria-busy={busy}
      >
        <header>
          <h2>{t("New GitHub repository", "새 GitHub 저장소")}</h2>
          <p>
            {t(
              "Create a repository in the GitHub account connected inside this VM, then connect it to this workspace.",
              "이 VM에 연결한 GitHub 개인 계정에 저장소를 만들고, 작업 공간에 연결해요.",
            )}
          </p>
        </header>
        <fieldset disabled={!canControl || busy || !!created}>
          <label>
            {t("Repository name", "저장소 이름")}
            <input
              required
              maxLength={100}
              value={name}
              onChange={(event) => {
                setName(event.target.value);
                changeRequest();
              }}
              placeholder="my-app"
            />
          </label>
          <label>
            {t("Description (optional)", "설명 (선택)")}
            <input
              maxLength={350}
              value={description}
              onChange={(event) => {
                setDescription(event.target.value);
                changeRequest();
              }}
            />
          </label>
          <label>
            {t("Visibility", "공개 범위")}
            <select
              value={privateRepository ? "private" : "public"}
              onChange={(event) => {
                setPrivateRepository(event.target.value === "private");
                changeRequest();
              }}
            >
              <option value="private">{t("Private", "비공개")}</option>
              <option value="public">{t("Public", "공개")}</option>
            </select>
          </label>
          <label>
            <input
              type="checkbox"
              checked={initialize}
              onChange={(event) => {
                setInitialize(event.target.checked);
                changeRequest();
              }}
            />
            {t("Initialize with a README", "README 파일로 초기화")}
          </label>
        </fieldset>
        {created && (
          <p role="status">
            {t(
              "Repository created. Connecting this workspace:",
              "저장소를 만들었어요. 작업 공간에 연결해요:",
            )}{" "}
            <a
              href={`https://github.com/${created.name}`}
              target="_blank"
              rel="noreferrer"
            >
              {created.name}
            </a>
          </p>
        )}
        {error && (
          <p className="am-error" role="alert">
            {error}
          </p>
        )}
        <footer>
          <button
            className="btn"
            type="button"
            disabled={busy}
            onClick={onClose}
          >
            {t("Cancel", "취소")}
          </button>
          <button
            className="btn primary"
            disabled={!canControl || busy || !name.trim()}
          >
            {busy
              ? t("Creating and connecting…", "저장소 생성·연결 중…")
              : created
                ? t("Retry connection", "연결 다시 시도")
                : t("Create and connect", "만들고 연결")}
          </button>
        </footer>
      </form>
    </AppDialog>
  );
}
