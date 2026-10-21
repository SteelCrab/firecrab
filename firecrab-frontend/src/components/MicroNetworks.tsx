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
export default function MicroNetworks() {
  const { t } = useI18n();
  const { setDetailOpen } = useScreenStatus();
  const [networks, setNetworks] = useState<MicroNetworkResponse[] | null>(null);
  const [name, setName] = useState("");
  const [subnetCidr, setSubnetCidr] = useState("");
  const [internetEnabled, setInternetEnabled] = useState(true);
  const [uplink, setUplink] = useState("");
  // Off is IPv4-only. On sends `ipv6AddressMode` (and an optional prefix);
  // the API then generates a per-host ULA /64 when the prefix is blank.
  const [ipv6Enabled, setIpv6Enabled] = useState(false);
  const [ipv6Cidr, setIpv6Cidr] = useState("");
  const [ipv6AddressMode, setIpv6AddressMode] = useState<Ipv6AddressMode>("slaac");
  const [interfaces, setInterfaces] = useState<string[]>([]);
  const [defaultUplink, setDefaultUplink] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [fieldErrors, setFieldErrors] = useState<ApiClientError | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
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
    setDetail(null);
    setDetailError(null);
    getMicroNetwork(selectedId)
      .then(setDetail)
      .catch((error) => setDetailError((error as Error).message));
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
    if (busyId || !window.confirm(t(`Delete MicroNetwork "${network.name}"?`, `MicroNetwork "${network.name}"을(를) 삭제할까요?`))) return;
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
      await updateMicroNetwork(network.id, { internetEnabled: !network.internetEnabled });
      await refresh();
      if (selectedId === network.id) setDetail(await getMicroNetwork(network.id));
    } catch (error) {
      setListError((error as Error).message);
    } finally {
      setBusyId(null);
    }
  };

  const fieldError = (field: string) => (
    <span className="field-error">{fieldErrors?.fieldError(field) ?? ""}</span>
  );

  const selectedNetwork = networks?.find((network) => network.id === selectedId) ?? null;
  const select = (id: string) => {
    setSelectedId(id);
    setDetailOpen(true);
  };

  return (
    <Screen
      view="networks"
      detail={selectedNetwork ? (
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
      ) : null}
      detailLabel={t("MicroNetwork detail", "MicroNetwork 상세")}
      detailKind={<><Network aria-hidden="true" />MicroNetwork</>}
      onCloseDetail={() => setSelectedId(null)}
    >
    <section className="panel">
      <h2 className="panel-title">MicroNetwork</h2>
      <form className="create-grid" onSubmit={handleSubmit}>
        <div className="field">
          <label htmlFor="mn-name">name</label>
          <input
            id="mn-name"
            placeholder="prod"
            value={name}
            onChange={(event) => setName(event.target.value)}
            required
            minLength={1}
            maxLength={64}
          />
          {fieldError("name")}
        </div>
        <div className="field">
          <label htmlFor="mn-subnet">subnet CIDR</label>
          <input
            id="mn-subnet"
            placeholder="172.31.0.0/24"
            value={subnetCidr}
            onChange={(event) => setSubnetCidr(event.target.value)}
            required
          />
          {fieldError("subnetCidr")}
        </div>
        <div className="field">
          <label htmlFor="mn-internet">{t("Internet", "인터넷")}</label>
          <select
            id="mn-internet"
            value={internetEnabled ? "on" : "off"}
            onChange={(event) => setInternetEnabled(event.target.value === "on")}
          >
            <option value="on">{t("Enabled (NAT)", "연결 (NAT)")}</option>
            <option value="off">{t("Blocked (internal only)", "차단 (내부 전용)")}</option>
          </select>
          <span className="field-error"></span>
        </div>
        <div className="field">
          <label htmlFor="mn-uplink">{t("Uplink", "업링크")}</label>
          <select
            id="mn-uplink"
            value={uplink}
            onChange={(event) => setUplink(event.target.value)}
          >
            <option value="">
              {defaultUplink
                ? t(`Auto (default route: ${defaultUplink})`, `자동 (기본 경로: ${defaultUplink})`)
                : t("Auto (default route)", "자동 (기본 경로)")}
            </option>
            {interfaces.map((iface) => (
              <option key={iface} value={iface}>
                {iface}
              </option>
            ))}
          </select>
          {fieldError("uplink")}
        </div>
        <div className="field">
          <label htmlFor="mn-ipv6-enable">IPv6</label>
          <select
            id="mn-ipv6-enable"
            value={ipv6Enabled ? "on" : "off"}
            onChange={(event) => setIpv6Enabled(event.target.value === "on")}
          >
            <option value="off">{t("Off (IPv4 only)", "꺼짐 (IPv4만)")}</option>
            <option value="on">{t("Enabled (auto ULA /64)", "연결 (자동 ULA /64)")}</option>
          </select>
          <span className="field-error"></span>
        </div>
        {ipv6Enabled && (
          <>
            <div className="field">
              <label htmlFor="mn-ipv6">{t("IPv6 prefix", "IPv6 프리픽스")}</label>
              <input
                id="mn-ipv6"
                placeholder={t("auto (ULA /64)", "자동 (ULA /64)")}
                value={ipv6Cidr}
                onChange={(event) => setIpv6Cidr(event.target.value)}
              />
              {fieldError("ipv6Cidr")}
            </div>
            <div className="field">
              <label htmlFor="mn-ipv6-mode">{t("IPv6 addressing", "IPv6 주소 할당")}</label>
              <select
                id="mn-ipv6-mode"
                value={ipv6AddressMode}
                onChange={(event) => setIpv6AddressMode(event.target.value as Ipv6AddressMode)}
              >
                <option value="slaac">SLAAC (RA)</option>
                <option value="dhcpv6">DHCPv6</option>
              </select>
              <span className="field-error"></span>
            </div>
          </>
        )}
        <div className="field">
          <label>&nbsp;</label>
          <button className="btn primary" type="submit" disabled={submitting}>
            {submitting ? t("Creating…", "생성 중…") : t("Create", "생성")}
          </button>
          <span className="field-error"></span>
        </div>
      </form>

      {listError && <div className="field-error">{listError}</div>}

      {networks === null ? (
        <div className="empty">{t("Loading…", "불러오는 중…")}</div>
      ) : networks.length === 0 ? (
        <div className="empty">{t("No MicroNetworks yet — create one above.", "MicroNetwork가 없습니다 — 위에서 생성하세요")}</div>
      ) : (
        <div className="table-scroll">
          <table className="vm-table">
            <thead>
              <tr>
                <th>name</th>
                <th>subnet CIDR</th>
                <th className="col-detail">gateway</th>
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
                  className={selectedId === network.id ? "selected" : undefined}
                  aria-selected={selectedId === network.id}
                  onClick={() => select(network.id)}
                >
                  <td className="name">{network.name}</td>
                  <td className="mono">{network.subnetCidr}</td>
                  <td className="mono col-detail">{network.gateway}</td>
                  <td className="mono col-detail">
                    {network.ipv6Cidr ?? t("Off", "꺼짐")}
                    {network.ipv6Egress && (
                      <>
                        <br />
                        {network.ipv6Egress === "nat66" ? "NAT66" : t("direct", "직접 라우팅")}
                      </>
                    )}
                  </td>
                  <td>{network.internetEnabled ? t("Enabled", "연결") : t("Blocked", "차단")}</td>
                  <td className="mono col-detail">{network.uplink ?? t("Auto", "자동")}</td>
                  <td className="mono col-detail">
                    {network.internetEnabled
                      ? `${network.subnetCidr} → ${network.uplink ?? (defaultUplink || t("Auto", "자동"))}`
                      : t("Off", "꺼짐")}
                  </td>
                  <td className="mono col-detail" title={network.id}>
                    {network.id.split("-")[0]}
                  </td>
                  <td className="actions">
                    <button
                      className="btn"
                      disabled={busyId === network.id}
                      title={
                        network.internetEnabled
                          ? t("Remove NAT and block outbound traffic", "NAT을 떼고 외부로 나가는 트래픽을 차단합니다")
                          : t("Attach NAT and allow outbound traffic", "NAT을 붙여 외부 통신을 허용합니다")
                      }
                      onClick={(event) => {
                        event.stopPropagation();
                        handleToggleInternet(network);
                      }}
                    >
                      {network.internetEnabled ? t("Block internet", "인터넷 차단") : t("Enable internet", "인터넷 연결")}
                    </button>
                    <button
                      className="btn danger"
                      disabled={busyId === network.id}
                      onClick={(event) => {
                        event.stopPropagation();
                        handleDelete(network);
                      }}
                    >
                      {t("Delete", "삭제")}
                    </button>
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
  if (error) return <>{head()}<div className="detail-body"><p className="field-error">{error}</p></div></>;
  if (!detail) return <>{head()}<div className="detail-body"><p className="detail-note">{t("Loading details…", "상세 불러오는 중…")}</p></div></>;

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
          <span className={nat.enabled ? "badge running" : "badge"}>{nat.enabled ? t("Internet on", "인터넷 연결") : t("Internet blocked", "인터넷 차단")}</span>
        </>,
      )}
      <div className="detail-body">
        <DetailSection title={t("Subnet", "서브넷")}>
          <dl className="props">
            <DetailProp label={t("Network ID", "네트워크 ID")} copy={detail.id}><span className="mono">{detail.id}</span></DetailProp>
            <DetailProp label="CIDR"><span className="mono">{subnet.cidr}</span></DetailProp>
            <DetailProp label={t("Gateway", "게이트웨이")}><span className="mono">{subnet.gateway}</span></DetailProp>
            <DetailProp label={t("Addresses", "주소")}>{subnet.allocatedAddresses}/{subnet.usableAddresses} {t("used", "사용 중")}</DetailProp>
            <DetailProp label="DHCP">{subnet.dhcp}</DetailProp>
            <DetailProp label="IPv6" wrap>
              {subnet.ipv6Cidr ? (
                <>
                  <span className="mono">{subnet.ipv6Cidr}</span> · gateway <span className="mono">{subnet.ipv6Gateway}</span>
                  <br />
                  {subnet.ipv6AddressMode === "dhcpv6" ? "DHCPv6" : "SLAAC (RA)"} ·{" "}
                  {subnet.ipv6Egress === "nat66"
                    ? t("NAT66 (unique-local prefix)", "NAT66 (unique-local 프리픽스)")
                    : t("direct (globally routable)", "직접 라우팅 (공인 프리픽스)")}
                </>
              ) : (
                t("Off", "꺼짐")
              )}
            </DetailProp>
            <DetailProp label={t("Bridge", "브릿지")}>
              <span className="mono">{bridge.name}</span> · TAP {bridge.attachedTaps} {t("attached", "개 연결")}
            </DetailProp>
          </dl>
        </DetailSection>

        <DetailSection title="NAT">
          <dl className="props">
            <DetailProp label={t("State", "상태")} wrap>
              {nat.enabled
                ? t("Enabled", "연결")
                : t("Internet blocked — no masquerading; outbound traffic is dropped", "인터넷 차단 — 마스커레이드 없음, 외부로 나가는 트래픽 drop")}
            </DetailProp>
            <DetailProp label={t("Source", "출발")}><span className="mono">{nat.sourceCidr}</span></DetailProp>
            {subnet.ipv6Cidr && (
              <DetailProp label={t("Source (IPv6)", "출발 (IPv6)")}>
                {nat.ipv6SourceCidr ? <span className="mono">{nat.ipv6SourceCidr}</span> : t("none — not translated", "없음 — 변환하지 않음")}
              </DetailProp>
            )}
            <DetailProp label={t("Uplink", "업링크")}><span className="mono">{nat.uplink || t("(no uplink)", "(uplink 없음)")}</span></DetailProp>
          </dl>
          <div className="field detail-field">
            <label htmlFor="mn-detail-uplink">{t("Change uplink", "업링크 변경")}</label>
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
                  ? t(`Auto (default route: ${defaultUplink})`, `자동 (기본 경로: ${defaultUplink})`)
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
            <li>{firewall.eastWestBlocked ? t("VM-to-VM blocked", "VM 간 차단") : t("VM-to-VM allowed", "VM 간 통신 허용")}</li>
            {firewall.crossNetworkBlocked && <li>{t("Cross-network blocked", "다른 네트워크 차단")}</li>}
            {firewall.antiSpoofing && <li>{t("IP/MAC spoofing blocked", "IP/MAC 위조 차단")}</li>}
            <li>
              <span className="detail-list-main">{t("Default egress", "기본 외부 통신")}</span>
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
                  <span className="detail-list-sub mono">{[vm.ipv4, vm.ipv6].filter(Boolean).join(", ") || t("no address", "주소 없음")}</span>
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
