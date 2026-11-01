import { useEffect, useState } from "react";
import type { MicroServiceSpec } from "../bindings";
import { fetchJson } from "../api/client";
import { listStorage } from "../api/pipeStorage";
import { useI18n } from "../i18n";
import { DetailProp } from "./ui/DetailSection";

/** Connection names are metadata; never request or render the referenced secret values. */
export default function ServiceConnections({
  spec,
}: {
  spec: MicroServiceSpec;
}) {
  const { t } = useI18n();
  const [names, setNames] = useState<Record<string, string>>({}),
    [storage, setStorage] = useState<string | null>(null);
  const queueKey = JSON.stringify(
    Object.values(spec.queues ?? {})
      .map((row) => row.queueId)
      .sort(),
  );
  useEffect(() => {
    let active = true;
    if (queueKey !== "[]")
      fetchJson<{ id: string; name: string }[]>("/api/queues")
        .then((rows) => {
          if (active && Array.isArray(rows))
            setNames(Object.fromEntries(rows.map((row) => [row.id, row.name])));
        })
        .catch(() => {
          if (active) setNames({});
        });
    return () => {
      active = false;
    };
  }, [queueKey]);
  useEffect(() => {
    let active = true;
    setStorage(null);
    if (spec.storageConnectionId)
      listStorage()
        .then((rows) => {
          if (active)
            setStorage(
              rows.find((row) => row.id === spec.storageConnectionId)?.name ??
                null,
            );
        })
        .catch(() => {});
    return () => {
      active = false;
    };
  }, [spec.storageConnectionId]);
  const bindings = Object.entries(spec.queues ?? {}),
    secrets = Object.keys(spec.secrets).length;
  return (
    <DetailProp label={t("Connections", "연결")}>
      <span className="chip-row">
        {bindings.map(([alias, row]) => (
          <a href="#/queues" className="chip" key={alias}>
            Queue {names[row.queueId] ?? alias}
          </a>
        ))}
        {spec.storageConnectionId && (
          <a href="#/pipe/storage" className="chip">
            S3 {storage ?? spec.storageConnectionId.slice(0, 8)}
          </a>
        )}
        {secrets > 0 && (
          <a href="#/secrets" className="chip">
            {t("Secret", "시크릿")} {secrets}
          </a>
        )}
        {!bindings.length &&
          !spec.storageConnectionId &&
          !secrets &&
          t("None", "없음")}
      </span>
    </DetailProp>
  );
}
