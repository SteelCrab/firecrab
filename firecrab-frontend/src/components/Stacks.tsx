import { useEffect, useState } from "react";
import { ApiClientError, fetchJson } from "../api/client";
import { useI18n } from "../i18n";
import Screen from "./layout/Screen";
import StackSource, { type Source } from "./StackSource";
import {
  ResourceGroups,
  nativeRecords,
  terraformStates,
  type StackInfo,
  type Ownership,
} from "./StackResources";
import { VIEW_ICONS } from "../viewIcons";
import "../styles/stacks.css";

type Row = { stack: StackInfo; source?: Source | null; error?: string };

export default function Stacks() {
  const { t } = useI18n();
  const [rows, setRows] = useState<Row[] | null>(null);
  const [error, setError] = useState("");
  const [owners, setOwners] = useState<Ownership[]>([]);
  const [ownershipError, setOwnershipError] = useState("");
  const states = terraformStates(owners);
  const StackIcon = VIEW_ICONS.stacks;
  useEffect(() => {
    let active = true;
    let timer = 0;
    const refresh = async () => {
      try {
        const stacks = await fetchJson<StackInfo[]>("/api/stacks");
        if (!Array.isArray(stacks)) throw new Error("Invalid stack response");
        const next = await Promise.all(
          stacks.map(async (stack) => {
            try {
              return {
                stack,
                source: await fetchJson<Source>(
                  `/api/stacks/${encodeURIComponent(stack.name)}/source`,
                ),
              };
            } catch (failure) {
              return failure instanceof ApiClientError && failure.status === 404
                ? { stack, source: null }
                : {
                    stack,
                    error:
                      failure instanceof Error
                        ? failure.message
                        : String(failure),
                  };
            }
          }),
        );
        let nextOwners: Ownership[] = [];
        let ownerError = "";
        try {
          nextOwners = await fetchJson<Ownership[]>("/api/iac/owners");
          if (!Array.isArray(nextOwners))
            throw new Error("Invalid ownership response");
        } catch (failure) {
          nextOwners = [];
          if (!(failure instanceof ApiClientError && failure.status === 404))
            ownerError =
              failure instanceof Error ? failure.message : String(failure);
        }
        if (active) {
          setOwners(nextOwners);
          setOwnershipError(ownerError);
          setRows(next);
          setError("");
        }
      } catch (failure) {
        if (active)
          setError(
            failure instanceof Error ? failure.message : String(failure),
          );
      }
      if (active) timer = window.setTimeout(() => void refresh(), 3000);
    };
    void refresh();
    return () => {
      active = false;
      window.clearTimeout(timer);
    };
  }, []);
  return (
    <Screen view="stacks" live={false}>
      {error && <p role="alert">{error}</p>}
      {rows === null && !error && <p>{t("Loading…", "불러오는 중…")}</p>}
      {rows?.length === 0 &&
        !error &&
        !ownershipError &&
        owners.length === 0 && (
          <section
            className="panel stack-setup"
            aria-label={t("Set up a Stack", "Stack 구성 방법")}
          >
            <h2>{t("Set up a Stack", "Stack 구성 방법")}</h2>
            <p>{t("No registered stacks.", "등록된 Stack이 없다.")}</p>
            <p>
              {t(
                "Use the Firecrab CLI connected to this host.",
                "현재 호스트에 연결한 Firecrab CLI에서 진행한다.",
              )}
            </p>
            <ol>
              <li>
                {t(
                  "Write the network and required MicroVMs or MicroServices in stack.yaml.",
                  "stack.yaml 파일에 네트워크와 필요한 MicroVM 또는 MicroService를 작성한다.",
                )}
              </li>
              <li>
                {t(
                  "Validate the file and register its Stack name.",
                  "파일을 검증하고 Stack 이름을 등록한다.",
                )}
                <pre>
                  firecrab stack validate --file stack.yaml{"\n"}firecrab stack
                  register --name demo
                </pre>
              </li>
              <li>
                {t(
                  "Review the planned changes, then apply them.",
                  "변경 계획을 확인한 뒤 적용한다.",
                )}
                <pre>
                  firecrab stack plan --file stack.yaml --json &gt; plan.json
                  {"\n"}firecrab stack apply --file stack.yaml --expected-plan
                  plan.json
                </pre>
              </li>
              <li>
                {t(
                  "The registered Stack appears here automatically.",
                  "등록된 Stack이 이 화면에 자동으로 표시된다.",
                )}
              </li>
            </ol>
            <p>
              {t(
                "Replace demo with the name in stack.yaml.",
                "예시의 demo는 stack.yaml의 name 값으로 바꾼다.",
              )}
            </p>
            <a
              href="https://github.com/SteelCrab/firecrab/blob/main/public-docs/guides/stacks.md#schema"
              target="_blank"
              rel="noopener noreferrer"
            >
              {t("View the YAML example", "YAML 작성 예시 보기")}
            </a>
          </section>
        )}
      {rows?.map(({ stack, source, error: sourceError }) => (
        <section
          className="panel stack-card"
          key={stack.id ?? stack.name}
          data-manager="stack"
          data-owner-id={stack.id}
        >
          <header className="stack-heading">
            <div className="stack-title">
              <StackIcon aria-hidden="true" />
              <h2>{stack.name}</h2>
            </div>
            <span className="stack-manager">Firecrab Stack</span>
          </header>
          <ResourceGroups records={nativeRecords(stack, owners)} />
          <StackSource name={stack.name} source={source} error={sourceError} />
        </section>
      ))}
      {ownershipError && (
        <p role="alert">
          {t("Ownership unavailable", "소유권 정보를 불러오지 못했다")}:{" "}
          {ownershipError}
        </p>
      )}
      {states.map(([id, records]) => (
        <section
          className="panel stack-card"
          key={id}
          data-manager="terraform"
          data-owner-id={id}
        >
          <header className="stack-heading">
            <div className="stack-title">
              <StackIcon aria-hidden="true" />
              <h2>{t("Terraform ownership", "Terraform 소유권")}</h2>
            </div>
            <span className="stack-manager">Terraform</span>
          </header>
          <dl className="stack-status stack-owner">
            <dt>{t("State owner ID", "상태 소유자 ID")}</dt>
            <dd className="mono">{id}</dd>
          </dl>
          <ResourceGroups records={records} />
        </section>
      ))}
    </Screen>
  );
}
