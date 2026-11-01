import PipeServiceMenu from "./PipeServiceMenu";
import { PanelRightOpen } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { FormEvent, ReactNode } from "react";
import type {
  Ipv6AddressMode,
  MicroNetworkDetailResponse,
  MicroNetworkResponse,
} from "../bindings";
import {
  ApiClientError,
  createMicroNetwork,
  deleteMicroNetwork,
  getMicroNetwork,
  getNetworkInfo,
  listMicroNetworks,
  updateMicroNetwork,
} from "../api/client";
import { Network } from "lucide-react";
import { useI18n } from "../i18n";
import Screen from "./layout/Screen";
import { useScreenStatus } from "./layout/screenStatus";
import { DetailHead, DetailProp, DetailSection } from "./ui/DetailSection";
import StatusBadge from "./ui/StatusBadge";

/**
 * MicroNetwork management (`public-docs/guides/networking.md`) — firecrab's own
 * virtual networks. Creating one reserves the CIDR, provisions its host
 * bridge, and gives it its own DHCP range and NAT rule; VMs then pick one on
 * the create form. Deleting is refused while VMs are still in it.
 */
export default function MicroNetworks({
  onUnsavedChange,
}: {
  onUnsavedChange?: (dirty: boolean) => void;
}) {
  const { t } = useI18n();
  const { setDetailOpen } = useScreenStatus();
  const [networks, setNetworks] = useState<MicroNetworkResponse[] | null>(null);
  const [editor, setEditor] = useState(false);
  const [name, setName] = useState("");
  const [subnetCidr, setSubnetCidr] = useState("");
  const [internetEnabled, setInternetEnabled] = useState(true);
  const [uplink, setUplink] = useState("");
  // Off is IPv4-only. On sends `ipv6AddressMode` (and an optional prefix);
  // the API then generates a per-host ULA /64 when the prefix is blank.
  const [ipv6Enabled, setIpv6Enabled] = useState(false);
  const [ipv6Cidr, setIpv6Cidr] = useState("");
  const [ipv6AddressMode, setIpv6AddressMode] =
    useState<Ipv6AddressMode>("slaac");
  const [interfaces, setInterfaces] = useState<string[]>([]);
  const [defaultUplink, setDefaultUplink] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [fieldErrors, setFieldErrors] = useState<ApiClientError | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const dirty =
    editor &&
    !!(name || subnetCidr || uplink || ipv6Enabled || !internetEnabled);
  useEffect(() => {
    onUnsavedChange?.(dirty);
    return () => onUnsavedChange?.(false);
  }, [dirty, onUnsavedChange]);
  const closeEditor = () => {
    if (submitting) return false;
    if (
      dirty &&
      !window.confirm(
        t("Discard unsaved changes?", "저장하지 않은 변경 사항을 취소할까요?"),
      )
    )
      return false;
    setName("");
    setSubnetCidr("");
    setUplink("");
    setIpv6Enabled(false);
    setIpv6Cidr("");
    setIpv6AddressMode("slaac");
    setInternetEnabled(true);
    setEditor(false);
    return true;
  };
  const selectedIdRef = useRef(selectedId);
  selectedIdRef.current = selectedId;
  const [detail, setDetail] = useState<MicroNetworkDetailResponse | null>(null);
  const [detailError, setDetailError] = useState<string | null>(null);

  // Refetched on every selection rather than derived from the list row: the
  // detail carries live counts (leases in use, TAPs attached) the list
  // doesn't have.
  useEffect(() => {
    if (!selectedId) {
      setDetail(null);
      return;
    }
    let active = true;
    setDetail(null);
    setDetailError(null);
    getMicroNetwork(selectedId)
      .then((value) => {
        if (active) setDetail(value);
      })
      .catch((error) => {
        if (active) setDetailError((error as Error).message);
      });
    return () => {
      active = false;
    };
  }, [selectedId]);

  const refresh = async () => {
    try {
      setNetworks(await listMicroNetworks());
      setListError(null);
    } catch (error) {
      setListError((error as Error).message);
    }
  };

  useEffect(() => {
    refresh();
    getNetworkInfo()
      .then((info) => {
        setInterfaces(info.interfaces ?? []);
        setDefaultUplink(info.uplink);
      })
      .catch(() => {
        setInterfaces([]);
      });
  }, []);

  const handleSubmit = async (event: FormEvent) => {
    event.preventDefault();
    if (submitting) return;

    setSubmitting(true);
    setFieldErrors(null);
    setListError(null);
    try {
      await createMicroNetwork({
        name: name.trim(),
        subnetCidr: subnetCidr.trim(),
        internetEnabled,
        ...(uplink ? { uplink } : {}),
        ...(ipv6Enabled
          ? {
              ipv6AddressMode,
              ...(ipv6Cidr.trim() ? { ipv6Cidr: ipv6Cidr.trim() } : {}),
            }
          : {}),
      });
      setName("");
      setSubnetCidr("");
      setInternetEnabled(true);
      setUplink("");
      setIpv6Enabled(false);
      setIpv6Cidr("");
      setIpv6AddressMode("slaac");
      await refresh();
      setEditor(false);
    } catch (error) {
      const client = error as ApiClientError;
      setFieldErrors(client);
      // 400 field maps already sit under the inputs. Helper-down / 500 would
      // otherwise leave the form looking idle.
      if (Object.keys(client.apiError?.fields ?? {}).length === 0) {
        setListError(client.message);
      }
    } finally {
      setSubmitting(false);
    }
  };

  const handleDelete = async (network: MicroNetworkResponse) => {
    if (
      busyId ||
      !window.confirm(
        t(
          `Delete MicroNetwork "${network.name}"?`,
          `"${network.name}" 네트워크를 삭제할까요?`,
        ),
      )
    )
      return;
    setBusyId(network.id);
    try {
      await deleteMicroNetwork(network.id);
      if (selectedId === network.id) setSelectedId(null);
      await refresh();
    } catch (error) {
      setListError((error as Error).message);
    } finally {
      setBusyId(null);
    }
  };

  // Reloads both the row (the list's badge) and the panel (its NAT line), so
  // the two can't disagree about a network that was just toggled.
  const handleToggleInternet = async (network: MicroNetworkResponse) => {
    if (busyId) return;
    setBusyId(network.id);
    try {
      await updateMicroNetwork(network.id, {
        internetEnabled: !network.internetEnabled,
      });
      await refresh();
      if (selectedId === network.id)
        setDetail(await getMicroNetwork(network.id));
    } catch (error) {
      setListError((error as Error).message);
    } finally {
      setBusyId(null);
    }
  };

  const fieldError = (field: string) => (
    <span className="field-error">{fieldErrors?.fieldError(field) ?? ""}</span>
  );

  const selectedNetwork =
    networks?.find((network) => network.id === selectedId) ?? null;
  const select = (id: string) => {
    if (editor && !closeEditor()) return;
    setSelectedId(id);
    setDetailOpen(true);
  };

  return (
    <Screen
      view="networks"
      actions={
        <>
          <button
            className="btn primary"
            disabled={submitting}
            onClick={() => {
              setEditor(true);
              setSelectedId(null);
              setDetailOpen(true);
            }}
          >
            {t("New network", "새 네트워크")}
          </button>
        </>
      }
      detail={
        editor ? (
          <section className="network-editor">
            <h2>{t("New network", "새 네트워크")}</h2>
            <form id="editor" data-editing onSubmit={handleSubmit}>
              {fieldErrors && (
                <p
                  id="network-editor-error"
                  role="alert"
                  className="field-error"
                >
                  {Object.values(fieldErrors.apiError?.fields ?? {}).join(
                    " ",
                  ) || fieldErrors.message}
                </p>
              )}
              <div className="field">
                <label htmlFor="mn-name">{t("Name", "이름")}</label>
                <input
                  id="mn-name"
                  value={name}
                  onChange={(event) => setName(event.target.value)}
                  required
                  minLength={1}
                  maxLength={64}
                  disabled={submitting}
                />
                {fieldError("name")}
              </div>
              <div className="field">
                <label htmlFor="mn-subnet">
                  {t("IPv4 subnet (CIDR)", "IPv4 서브넷 (CIDR)")}
                </label>
                <input
                  id="mn-subnet"
                  placeholder="172.31.0.0/24"
                  value={subnetCidr}
                  onChange={(event) => setSubnetCidr(event.target.value)}
                  required
                  disabled={submitting}
                />
                {fieldError("subnetCidr")}
              </div>
              <fieldset
                className="network-choice segmented"
                role="radiogroup"
                aria-label={t("Internet enabled", "인터넷 연결")}
                disabled={submitting}
              >
                <legend>{t("Internet enabled", "인터넷 연결")}</legend>
                {[
                  { value: true, label: t("Enabled", "허용") },
                  { value: false, label: t("Blocked", "차단") },
                ].map((option) => (
                  <label key={String(option.value)}>
                    <input
                      type="radio"
                      name="internetEnabled"
                      value={String(option.value)}
                      checked={internetEnabled === option.value}
                      onChange={() => setInternetEnabled(option.value)}
                    />
                    <span>{option.label}</span>
                  </label>
                ))}
              </fieldset>
              <div className="field">
                <label htmlFor="mn-uplink">
                  {t("Uplink", "업링크")}{" "}
                  <span className="field-optional">
                    {t("Optional", "선택")}
                  </span>
                </label>
                <select
                  id="mn-uplink"
                  value={uplink}
                  disabled={submitting}
                  onChange={(event) => setUplink(event.target.value)}
                >
                  <option value="">
                    {defaultUplink
                      ? t(
                          `Default (${defaultUplink})`,
                          `기본값 (${defaultUplink})`,
                        )
                      : t("Default interface", "기본 인터페이스")}
                  </option>
                  {interfaces.map((value) => (
                    <option value={value} key={value}>
                      {value}
                    </option>
                  ))}
                </select>
                {fieldError("uplink")}
              </div>
              <div className="field">
                <label htmlFor="mn-ipv6">
                  {t("IPv6 prefix (optional)", "IPv6 프리픽스 (선택)")}
                </label>
                <input
                  id="mn-ipv6"
                  placeholder={t(
                    "Automatic ULA /64 when IPv6 is enabled",
                    "IPv6 사용 시 자동 ULA /64",
                  )}
                  value={ipv6Cidr}
                  disabled={submitting}
                  onChange={(event) => {
                    setIpv6Cidr(event.target.value);
                    if (event.target.value.trim()) setIpv6Enabled(true);
                  }}
                />
                {fieldError("ipv6Cidr")}
              </div>
              <fieldset
                className="network-choice chips"
                role="radiogroup"
                aria-label={t("IPv6 address mode", "IPv6 주소 할당")}
                disabled={submitting}
              >
                <legend>{t("IPv6 address mode", "IPv6 주소 할당")}</legend>
                {[
                  { value: "off", label: t("Default", "기본값") },
                  { value: "slaac", label: "SLAAC" },
                  { value: "dhcpv6", label: "DHCPv6" },
                ].map((option) => (
                  <label key={option.value}>
                    <input
                      type="radio"
                      name="ipv6AddressMode"
                      value={option.value}
                      checked={
                        (ipv6Enabled ? ipv6AddressMode : "off") === option.value
                      }
                      onChange={() => {
                        setIpv6Enabled(option.value !== "off");
                        if (option.value !== "off")
                          setIpv6AddressMode(option.value as Ipv6AddressMode);
                      }}
                    />
                    <span>{option.label}</span>
                  </label>
                ))}
              </fieldset>
              <p className="poll-note">
                {t(
                  "Default uses IPv4 only. SLAAC or DHCPv6 also enables an IPv6 network.",
                  "기본으로 IPv4만 사용합니다. SLAAC나 DHCPv6를 선택하면 IPv6도 사용할 수 있습니다.",
                )}
              </p>
              <div className="form-footer">
                <button
                  className="btn ghost"
                  type="button"
                  disabled={submitting}
                  onClick={closeEditor}
                >
                  {t("Cancel", "취소")}
                </button>
                <button
                  className="btn primary"
                  type="submit"
                  disabled={submitting}
                >
                  {submitting ? t("Creating…", "생성 중…") : t("Save", "저장")}
                </button>
              </div>
            </form>
          </section>
        ) : selectedNetwork ? (
          <MicroNetworkDetail
            name={selectedNetwork.name}
            detail={detail}
            error={detailError}
            storedUplink={selectedNetwork.uplink}
            interfaces={interfaces}
            defaultUplink={defaultUplink}
            busy={busyId === selectedNetwork.id}
            onSaveUplink={async (next) => {
              if (!selectedId || !detail || busyId) return;
              const id = selectedId;
              const internetEnabled = detail.nat.enabled;
              setBusyId(id);
              try {
                await updateMicroNetwork(id, {
                  internetEnabled,
                  uplink: next,
                });
                await refresh();
                if (selectedIdRef.current !== id) return;
                const nextDetail = await getMicroNetwork(id);
                if (selectedIdRef.current === id) setDetail(nextDetail);
              } catch (error) {
                setListError((error as Error).message);
              } finally {
                setBusyId(null);
              }
            }}
          />
        ) : null
      }
      detailLabel={t("MicroNetwork detail", "MicroNetwork 상세")}
      detailKind={
        <>
          <Network aria-hidden="true" />
          MicroNetwork
        </>
      }
      onCloseDetail={() => {
        if (editor && !closeEditor()) return false;
        setSelectedId(null);
        return true;
      }}
    >
      <section className="panel">
        {listError && <div className="field-error">{listError}</div>}

        {networks === null ? (
          <div className="empty">{t("Loading…", "불러오는 중…")}</div>
        ) : networks.length === 0 ? (
          <div className="empty">
            {t(
              "No MicroNetworks yet — create one above.",
              "MicroNetwork가 없습니다. 위에서 새로 만드세요.",
            )}
          </div>
        ) : (
          <div className="table-scroll">
            <table className="vm-table network-table">
              <thead>
                <tr>
                  <th>{t("Name", "이름")}</th>
                  <th>{t("Subnet CIDR", "서브넷 CIDR")}</th>
                  <th className="col-detail">{t("Gateway", "게이트웨이")}</th>
                  <th className="col-detail">IPv6</th>
                  <th>{t("Internet", "인터넷")}</th>
                  <th className="col-detail">{t("Uplink", "업링크")}</th>
                  <th className="col-detail">NAT</th>
                  <th className="col-detail">id</th>
                  <th className="actions">{t("Actions", "작업")}</th>
                </tr>
              </thead>
              <tbody>
                {networks.map((network) => (
                  <tr
                    key={network.id}
                    className={
                      selectedId === network.id ? "selected" : undefined
                    }
                    aria-selected={selectedId === network.id}
                    onClick={() => select(network.id)}
                  >
                    <td className="name">
                      <button
                        className="link-button"
                        onClick={(event) => {
                          event.stopPropagation();
                          select(network.id);
                        }}
                      >
                        {network.name}
                      </button>
                    </td>
                    <td className="mono">{network.subnetCidr}</td>
                    <td className="mono col-detail">{network.gateway}</td>
                    <td className="mono col-detail">
                      {network.ipv6Cidr ?? t("Off", "꺼짐")}
                      {network.ipv6Egress && (
                        <>
                          <br />
                          {network.ipv6Egress === "nat66"
                            ? "NAT66"
                            : t("direct", "직접 라우팅")}
                        </>
                      )}
                    </td>
                    <td>
                      {network.internetEnabled
                        ? t("Enabled", "연결")
                        : t("Blocked", "차단")}
                    </td>
                    <td className="mono col-detail">
                      {network.uplink ?? t("Auto", "자동")}
                    </td>
                    <td className="mono col-detail">
                      {network.internetEnabled
                        ? `${network.subnetCidr} → ${network.uplink ?? (defaultUplink || t("Auto", "자동"))}`
                        : t("Off", "꺼짐")}
                    </td>
                    <td className="mono col-detail" title={network.id}>
                      {network.id.split("-")[0]}
                    </td>
                    <td
                      className="actions"
                      onClick={(event) => event.stopPropagation()}
                    >
                      <button
                        className="icon-btn"
                        type="button"
                        title={t("Details", "상세 보기")}
                        aria-label={t("Details", "상세 보기")}
                        onClick={() => select(network.id)}
                      >
                        <PanelRightOpen aria-hidden="true" />
                      </button>
                      <PipeServiceMenu
                        label={t(
                          `Actions for ${network.name}`,
                          `${network.name} 작업`,
                        )}
                        disabled={busyId === network.id}
                        items={[
                          {
                            label: network.internetEnabled
                              ? t("Block internet", "인터넷 차단")
                              : t("Enable internet", "인터넷 연결"),
                            onSelect: () => void handleToggleInternet(network),
                          },
                          {
                            label: t("Delete", "삭제"),
                            danger: true,
                            onSelect: () => void handleDelete(network),
                          },
                        ]}
                      />
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>
    </Screen>
  );
}

/** One network's services, shown in the detail panel. */
function MicroNetworkDetail({
  name,
  detail,
  error,
  storedUplink,
  interfaces,
  defaultUplink,
  busy,
  onSaveUplink,
}: {
  name: string;
  detail: MicroNetworkDetailResponse | null;
  error: string | null;
  storedUplink: string | null;
  interfaces: string[];
  defaultUplink: string;
  busy: boolean;
  onSaveUplink: (uplink: string) => Promise<void>;
}) {
  const { t } = useI18n();
  const head = (meta?: ReactNode) => <DetailHead title={name} meta={meta} />;
  if (error)
    return (
      <>
        {head()}
        <div className="detail-body">
          <p className="field-error">{error}</p>
        </div>
      </>
    );
  if (!detail)
    return (
      <>
        {head()}
        <div className="detail-body">
          <p className="detail-note">
            {t("Loading details…", "상세 불러오는 중…")}
          </p>
        </div>
      </>
    );

  const { subnet, bridge, nat, firewall } = detail;
  const pickerInterfaces =
    storedUplink && !interfaces.includes(storedUplink)
      ? [storedUplink, ...interfaces]
      : interfaces;
  return (
    <>
      {head(
        <>
          <span className="mono">{subnet.cidr}</span>
          <span className={nat.enabled ? "badge running" : "badge"}>
            {nat.enabled
              ? t("Internet on", "인터넷 연결")
              : t("Internet blocked", "인터넷 차단")}
          </span>
        </>,
      )}
      <div className="detail-body">
        <DetailSection title={t("Subnet", "서브넷")}>
          <dl className="props">
            <DetailProp label={t("Network ID", "네트워크 ID")} copy={detail.id}>
              <span className="mono">{detail.id}</span>
            </DetailProp>
            <DetailProp label="CIDR">
              <span className="mono">{subnet.cidr}</span>
            </DetailProp>
            <DetailProp label={t("Gateway", "게이트웨이")}>
              <span className="mono">{subnet.gateway}</span>
            </DetailProp>
            <DetailProp label={t("Addresses", "주소")}>
              {subnet.allocatedAddresses}/{subnet.usableAddresses}{" "}
              {t("used", "사용 중")}
            </DetailProp>
            <DetailProp label="DHCP">{subnet.dhcp}</DetailProp>
            <DetailProp label="IPv6" wrap>
              {subnet.ipv6Cidr ? (
                <>
                  <span className="mono">{subnet.ipv6Cidr}</span> · gateway{" "}
                  <span className="mono">{subnet.ipv6Gateway}</span>
                  <br />
                  {subnet.ipv6AddressMode === "dhcpv6"
                    ? "DHCPv6"
                    : "SLAAC (RA)"}{" "}
                  ·{" "}
                  {subnet.ipv6Egress === "nat66"
                    ? t(
                        "NAT66 (unique-local prefix)",
                        "NAT66 (unique-local 프리픽스)",
                      )
                    : t(
                        "direct (globally routable)",
                        "직접 라우팅 (공인 IPv6 주소 대역)",
                      )}
                </>
              ) : (
                t("Off", "꺼짐")
              )}
            </DetailProp>
            <DetailProp label={t("Bridge", "브릿지")}>
              <span className="mono">{bridge.name}</span> · TAP{" "}
              {bridge.attachedTaps} {t("attached", "개 연결")}
            </DetailProp>
          </dl>
        </DetailSection>

        <DetailSection title="NAT">
          <dl className="props">
            <DetailProp label={t("State", "상태")} wrap>
              {nat.enabled
                ? t("Enabled", "연결")
                : t(
                    "Internet blocked — no masquerading; outbound traffic is dropped",
                    "인터넷 차단 — IP 주소 변환 없이 외부로 나가는 트래픽 차단",
                  )}
            </DetailProp>
            <DetailProp label={t("Source", "출발")}>
              <span className="mono">{nat.sourceCidr}</span>
            </DetailProp>
            {subnet.ipv6Cidr && (
              <DetailProp label={t("Source (IPv6)", "출발 (IPv6)")}>
                {nat.ipv6SourceCidr ? (
                  <span className="mono">{nat.ipv6SourceCidr}</span>
                ) : (
                  t("none — not translated", "없음 — 변환하지 않음")
                )}
              </DetailProp>
            )}
            <DetailProp label={t("Uplink", "업링크")}>
              <span className="mono">
                {nat.uplink || t("(no uplink)", "(uplink 없음)")}
              </span>
            </DetailProp>
          </dl>
          <div className="field detail-field">
            <label htmlFor="mn-detail-uplink">
              {t("Change uplink", "업링크 변경")}
            </label>
            <select
              id="mn-detail-uplink"
              value={storedUplink ?? ""}
              disabled={busy}
              onChange={(event) => {
                void onSaveUplink(event.target.value);
              }}
            >
              <option value="">
                {defaultUplink
                  ? t(
                      `Auto (default route: ${defaultUplink})`,
                      `자동 (기본 경로: ${defaultUplink})`,
                    )
                  : t("Auto (default route)", "자동 (기본 경로)")}
              </option>
              {pickerInterfaces.map((iface) => (
                <option key={iface} value={iface}>
                  {iface}
                </option>
              ))}
            </select>
          </div>
        </DetailSection>

        <DetailSection title={t("Firewall", "방화벽")}>
          <ul className="detail-list">
            <li>
              {firewall.eastWestBlocked
                ? t("VM-to-VM blocked", "VM 간 차단")
                : t("VM-to-VM allowed", "VM 간 통신 허용")}
            </li>
            {firewall.crossNetworkBlocked && (
              <li>{t("Cross-network blocked", "다른 네트워크 차단")}</li>
            )}
            {firewall.antiSpoofing && (
              <li>{t("IP/MAC spoofing blocked", "IP/MAC 위조 차단")}</li>
            )}
            <li>
              <span className="detail-list-main">
                {t("Default egress", "기본 외부 통신")}
              </span>
              <span className="detail-list-sub">{firewall.defaultEgress}</span>
            </li>
          </ul>
        </DetailSection>

        <DetailSection title={t("Member VMs", "소속 VM")}>
          {detail.vms.length === 0 ? (
            <p className="detail-note">{t("None", "없음")}</p>
          ) : (
            <ul className="detail-list">
              {detail.vms.map((vm) => (
                <li key={vm.id}>
                  <span className="detail-list-main">{vm.name}</span>
                  <span className="detail-list-sub mono">
                    {[vm.ipv4, vm.ipv6].filter(Boolean).join(", ") ||
                      t("no address", "주소 없음")}
                  </span>
                  <StatusBadge state={vm.state} />
                </li>
              ))}
            </ul>
          )}
        </DetailSection>
      </div>
    </>
  );
}
