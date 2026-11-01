import { useEffect, useRef, useState } from "react";
import { fetchJson } from "../../api/client";
import {
  CODE,
  equal,
  remember,
  restore,
  runtime,
  uuid,
  write,
} from "../../api/code";
import type { Runtime, Workspace } from "../../api/code";
import { current } from "./hooks";
import { useI18n } from "../../i18n";

type Placement = {
  template: string;
  microNetworkId: string;
  storageRoot: string;
};
type Pending = {
  workspace: string;
  revision: number;
  action: "create" | "start" | "stop";
  before: { vm: string; operation: string } | null;
  placement: Placement | null;
};
type Catalog = {
  images: { alias: string; installed: boolean; minDiskGb: number }[];
  networks: { id: string; name: string; internetEnabled: boolean }[];
  storage: { id: string; kind: string }[];
};
export function useRuntime(
  w: Workspace,
  r: Runtime | null,
  refresh: () => void,
  onDirty: (v: boolean) => void,
) {
  const key = `firecrab.code.runtime.react.${w.id}`;
  const [catalog, setCatalog] = useState<Catalog | null>(null),
    [choice, setChoice] = useState<Placement | null>(null),
    [epoch, reload] = useState(0);
  const [pending, setPending] = useState<Pending | null>(() => {
    const p = restore<Pending>(key);
    return p?.workspace === w.id &&
      p.revision > 0 &&
      ["create", "start", "stop"].includes(p.action)
      ? p
      : null;
  });
  const [busy, setBusy] = useState(false),
    [error, setError] = useState(""),
    [retry, setRetry] = useState(false);
  const lock = useRef(false),
    alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  useEffect(() => {
    onDirty(busy || !!pending);
    return () => onDirty(false);
  }, [busy, pending, onDirty]);
  useEffect(() => {
    const controller = new AbortController();
    void Promise.all([
      fetchJson<Catalog["images"]>("/api/images", {
        signal: controller.signal,
      }),
      fetchJson<Catalog["networks"]>("/api/micro-networks", {
        signal: controller.signal,
      }),
      fetchJson<Catalog["storage"]>("/api/storage/roots", {
        signal: controller.signal,
      }),
    ])
      .then(([images, networks, storage]) => {
        if (!controller.signal.aborted)
          setCatalog({ images, networks, storage });
      })
      .catch(() => {
        if (!controller.signal.aborted) setCatalog(null);
      });
    return () => controller.abort();
  }, [epoch]);
  const images = (catalog?.images ?? []).filter(
    (i) =>
      i.installed &&
      i.alias !== "__microboot" &&
      i.minDiskGb <= w.spec.resources.diskGb,
  );
  const placement = choice ?? {
    template:
      images.find((i) => i.alias.startsWith("ubuntu"))?.alias ??
      images[0]?.alias ??
      "",
    microNetworkId:
      catalog?.networks.find((n) => n.internetEnabled && uuid(n.id))?.id ?? "",
    storageRoot:
      catalog?.storage.find((s) => s.kind === "default")?.id ??
      catalog?.storage[0]?.id ??
      "",
  };
  const valid =
    images.some((i) => i.alias === placement.template) &&
    !!catalog?.networks.some(
      (n) =>
        n.id === placement.microNetworkId && n.internetEnabled && uuid(n.id),
    ) &&
    !!catalog?.storage.some((s) => s.id === placement.storageRoot);
  const moving =
    !!r &&
    ["queued", "provisioning", "preparing", "stopping"].includes(r.phase);
  function clear() {
    sessionStorage.removeItem(key);
    setPending(null);
    setRetry(false);
  }
  function admitted(p: Pending, next: Runtime | null) {
    if (
      !next ||
      next.workspaceId !== w.id ||
      next.configurationRevision !== p.revision ||
      next.action !== (p.action === "stop" ? "stop" : "start")
    )
      return false;
    if (p.before)
      return (
        next.vmId === p.before.vm && next.operationId !== p.before.operation
      );
    return (
      !!p.placement &&
      next.placement.template.name === p.placement.template &&
      next.placement.microNetworkId === p.placement.microNetworkId &&
      next.placement.storageRoot === p.placement.storageRoot
    );
  }
  async function dispatch(p: Pending) {
    remember(key, p);
    setPending(p);
    setRetry(false);
    try {
      const result = await write<Runtime>(
        `${CODE}/${w.id}/runtime${p.action === "create" ? "" : `/${p.action}`}`,
        { expectedRevision: p.revision, ...(p.placement ?? {}) },
      );
      if (!admitted(p, result)) throw new Error("unknown");
      clear();
    } catch {
      const observed = await runtime(w.id).catch(() => undefined);
      if (observed && admitted(p, observed)) clear();
      else if (alive.current) setError("unknown");
    }
    refresh();
  }
  async function act(action: "create" | "start" | "stop") {
    if (lock.current || pending || moving || (action === "create" && !valid))
      return;
    lock.current = true;
    setBusy(true);
    setError("");
    try {
      const saved = await current(w);
      if (
        action === "create" &&
        saved.connections.github !== "present_unverified"
      )
        throw new Error("changed");
      const before = await runtime(w.id);
      if (!equal(before, r)) throw new Error("changed");
      await dispatch({
        workspace: w.id,
        revision: w.revision,
        action,
        before: r ? { vm: r.vmId, operation: r.operationId } : null,
        placement: action === "create" ? placement : null,
      });
    } catch {
      if (alive.current) setError("changed");
      refresh();
    } finally {
      lock.current = false;
      if (alive.current) setBusy(false);
    }
  }
  async function recover(resend = false) {
    if (lock.current || !pending) return;
    lock.current = true;
    setBusy(true);
    setError("");
    try {
      await current(w);
      if (w.revision !== pending.revision) throw new Error("changed");
      const next = await runtime(w.id);
      if (admitted(pending, next)) {
        clear();
        refresh();
        return;
      }
      const same = pending.before
        ? next?.vmId === pending.before.vm &&
          next?.operationId === pending.before.operation
        : next === null;
      if (!same) throw new Error("changed");
      if (resend) await dispatch(pending);
      else setRetry(true);
    } catch {
      if (alive.current) setError("changed");
      refresh();
    } finally {
      lock.current = false;
      if (alive.current) setBusy(false);
    }
  }
  return {
    catalog,
    images,
    placement,
    setChoice,
    valid,
    pending,
    busy,
    moving,
    error,
    retry,
    act,
    recover,
    reload: () => reload((n) => n + 1),
  };
}
export default function RuntimeSettings({
  control,
  r,
}: {
  control: ReturnType<typeof useRuntime>;
  r: Runtime | null;
}) {
  const { t } = useI18n();
  const c = control;
  return (
    <section className="code-section code-runtime-controls">
      <h3>{t("MicroVM", "MicroVM")}</h3>
      <p className="code-runtime-status">
        {r?.phase ?? t("Not provisioned", "생성 전")}
      </p>
      {!r && (
        <div className="code-fields">
          <label>
            {t("Installed image", "설치된 이미지")}
            <select
              value={c.placement.template}
              disabled={c.busy || !!c.pending}
              onChange={(e) =>
                c.setChoice({ ...c.placement, template: e.target.value })
              }
            >
              {c.images.map((i) => (
                <option key={i.alias}>{i.alias}</option>
              ))}
            </select>
          </label>
          <label>
            MicroNetwork
            <select
              value={c.placement.microNetworkId}
              disabled={c.busy || !!c.pending}
              onChange={(e) =>
                c.setChoice({ ...c.placement, microNetworkId: e.target.value })
              }
            >
              {c.catalog?.networks
                .filter((n) => n.internetEnabled)
                .map((n) => (
                  <option key={n.id} value={n.id}>
                    {n.name}
                  </option>
                ))}
            </select>
          </label>
          <label>
            {t("Storage root", "스토리지 루트")}
            <select
              value={c.placement.storageRoot}
              disabled={c.busy || !!c.pending}
              onChange={(e) =>
                c.setChoice({ ...c.placement, storageRoot: e.target.value })
              }
            >
              {c.catalog?.storage.map((s) => (
                <option key={s.id}>{s.id}</option>
              ))}
            </select>
          </label>
          <button className="btn" disabled={c.busy} onClick={c.reload}>
            {t("Reload placement", "배치 다시 불러오기")}
          </button>
        </div>
      )}
      {r && ["running", "paused"].includes(r.vmState ?? "") && !c.moving && (
        <button
          className="btn"
          disabled={c.busy || !!c.pending}
          onClick={() => void c.act("stop")}
        >
          {t("Stop MicroVM", "MicroVM 중지")}
        </button>
      )}
      {c.error && (
        <p role="alert">
          {c.error === "unknown"
            ? t(
                "The request result is unknown. Check before retrying.",
                "요청 결과를 확인하지 못했습니다. 다시 시도하기 전에 확인해 주세요.",
              )
            : t(
                "The workspace or runtime changed. Refresh before continuing.",
                "워크스페이스나 실행 상태가 변경되었습니다. 새로고침해 주세요.",
              )}
        </p>
      )}
      {c.pending && (
        <div className="code-actions">
          <button
            className="btn"
            disabled={c.busy}
            onClick={() => void c.recover()}
          >
            {t("Check operation", "작업 확인")}
          </button>
          {c.retry && (
            <button
              className="btn"
              disabled={c.busy}
              onClick={() => void c.recover(true)}
            >
              {t("Retry same request", "같은 요청 재시도")}
            </button>
          )}
        </div>
      )}
    </section>
  );
}
