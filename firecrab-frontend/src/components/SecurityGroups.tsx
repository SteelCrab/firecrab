import { useEffect, useRef, useState } from "react";
import type { FormEvent } from "react";
import { Shield } from "lucide-react";
import type { MicroNetworkResponse, SecurityGroupResponse, SecurityGroupRule } from "../bindings";
import {
  ApiClientError, createSecurityGroup, deleteSecurityGroup, listMicroNetworks,
  listSecurityGroups, replaceSecurityGroupRules,
} from "../api/client";
import { useI18n } from "../i18n";
import { viewHash } from "../navigation";
import Screen from "./layout/Screen";
import { useScreenStatus } from "./layout/screenStatus";

export default function SecurityGroups() {
  const { t } = useI18n();
  const { setDetailOpen } = useScreenStatus();
  const [groups, setGroups] = useState<SecurityGroupResponse[] | null>(null);
  const [networks, setNetworks] = useState<MicroNetworkResponse[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // undefined closes the editor; null creates an empty policy first.
  const [editor, setEditor] = useState<SecurityGroupResponse | null | undefined>(undefined);

  useEffect(() => {
    let active = true;
    Promise.all([listSecurityGroups(), listMicroNetworks()])
      .then(([rows, nets]) => { if (active) { setGroups(rows); setNetworks(nets); } })
      .catch((failure) => { if (active) setError((failure as Error).message); });
    return () => { active = false; };
  }, []);

  const refresh = async () => {
    if (busy) return;
    setBusy(true); setError(null);
    try {
      const [rows, nets] = await Promise.all([listSecurityGroups(), listMicroNetworks()]);
      setGroups(rows); setNetworks(nets);
    } catch (failure) {
      setError((failure as Error).message);
    } finally { setBusy(false); }
  };

  const saved = (group: SecurityGroupResponse) => {
    setGroups((rows) => [...(rows ?? []).filter((row) => row.id !== group.id), group]
      .sort((a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id)));
    setEditor(group);
  };

  const remove = async (group: SecurityGroupResponse) => {
    if (busy || !window.confirm(t(`Delete security group "${group.name}"?`, `보안그룹 "${group.name}"을(를) 삭제할까요?`))) return;
    setBusy(true); setError(null);
    try {
      await deleteSecurityGroup(group.id);
      setGroups((rows) => rows?.filter((row) => row.id !== group.id) ?? []);
    } catch (failure) {
      setError((failure as Error).message);
    } finally { setBusy(false); }
  };

  const open = (group: SecurityGroupResponse | null) => {
    setError(null); setEditor(group); setDetailOpen(true);
  };
  const networkName = (id: string) => networks.find((network) => network.id === id)?.name ?? id;
  const close = () => setEditor(undefined);

  return (
    <Screen
      view="security-groups"
      detail={editor === undefined ? null : <div className="detail-body detail-embed">
        {editor === null
          ? <CreateGroupForm networks={networks} onSaved={saved} onCancel={close} />
          : <RulesForm key={editor.id} group={editor} networkName={networkName(editor.microNetworkId)} onSaved={saved} onCancel={close} />}
      </div>}
      detailLabel={t("Security group detail", "보안그룹 상세")}
      detailKind={<><Shield aria-hidden="true" />{t("Security group", "보안그룹")}</>}
      onCloseDetail={close}
    >
      <section className="panel" aria-labelledby="security-groups-heading">
        <h2 className="panel-title" id="security-groups-heading">{t("Security groups", "보안그룹")}</h2>
        <p className="poll-note">{t("Allow specific IPv4 connections on a MicroNetwork. Other new connections are denied.", "MicroNetwork에서 지정한 IPv4 연결을 허용한다. 그 외의 새 연결은 차단한다.")}</p>
        <div className="service-actions">
          <button className="btn primary" disabled={busy || groups === null || editor !== undefined} onClick={() => open(null)}>{t("New security group", "보안그룹 추가")}</button>
          <button className="btn" disabled={busy || editor !== undefined} onClick={refresh}>{t("Refresh", "새로 고침")}</button>
        </div>
        {error && <p className="field-error" role="alert">{error}</p>}
        {groups === null ? !error && <p className="empty">{t("Loading…", "불러오는 중…")}</p>
          : groups.length === 0 ? <p className="empty">{t("No security groups yet.", "저장된 보안그룹이 없다.")}</p>
          : <ul className="security-group-list" aria-label={t("Security groups", "보안그룹")}>
            {groups.map((group) => {
              const inbound = group.rules.filter((rule) => rule.direction === "ingress").length;
              const outbound = group.rules.length - inbound;
              return <li key={group.id} className={editor?.id === group.id ? "is-selected" : undefined}>
              <div className="security-group-summary">
                <strong>{group.name}</strong>
                <span>{networkName(group.microNetworkId)}</span>
                <span className="poll-note">{t(
                  `Inbound ${inbound} · Outbound ${outbound}`,
                  `인바운드 ${inbound} · 아웃바운드 ${outbound}`,
                )}</span>
              </div>
              <div className="service-actions">
                <button className="btn" disabled={busy || editor !== undefined} onClick={() => open(group)} aria-label={t(`Edit rules for ${group.name}`, `${group.name} 규칙 편집`)}>{t("Edit rules", "규칙 편집")}</button>
                <button className="btn danger" disabled={busy || editor !== undefined} onClick={() => remove(group)} aria-label={t(`Delete ${group.name}`, `${group.name} 삭제`)}>{t("Delete", "삭제")}</button>
              </div>
            </li>; })}
          </ul>}
      </section>
    </Screen>
  );
}

function CreateGroupForm({ networks, onSaved, onCancel }: {
  networks: MicroNetworkResponse[]; onSaved: (group: SecurityGroupResponse) => void; onCancel: () => void;
}) {
  const { t } = useI18n();
  const [name, setName] = useState("");
  const [network, setNetwork] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<Error | null>(null);
  const eligible = networks.filter((row) => !row.ipv6Cidr);
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (saving) return;
    setSaving(true); setError(null);
    try { onSaved(await createSecurityGroup({ name: name.trim(), microNetworkId: network })); }
    catch (failure) { setError(failure as Error); }
    finally { setSaving(false); }
  };
  return <section className="panel" aria-labelledby="security-group-create-heading" data-editing>
    <h2 className="panel-title" id="security-group-create-heading">{t("New security group", "보안그룹 추가")}</h2>
    {error && <p className="field-error" role="alert">{error.message}</p>}
    <form className="security-group-form" onSubmit={submit}>
      <div className="field"><label htmlFor="security-group-name">{t("Name", "이름")}</label><input id="security-group-name" value={name} maxLength={64} required disabled={saving} onChange={(event) => setName(event.target.value)} placeholder="orders-security-group" /></div>
      <div className="field"><label htmlFor="security-group-network">MicroNetwork</label><select id="security-group-network" value={network} required disabled={saving} onChange={(event) => setNetwork(event.target.value)}>
        <option value="">{t("Select an IPv4-only network", "IPv4 전용 네트워크 선택")}</option>
        {networks.map((row) => <option key={row.id} value={row.id} disabled={!!row.ipv6Cidr}>{row.name} · {row.subnetCidr}{row.ipv6Cidr ? t(" (IPv6 unsupported)", " (IPv6 지원 안 함)") : ""}</option>)}
      </select></div>
      {eligible.length === 0 && <p className="poll-note">{t("Create an IPv4-only MicroNetwork first.", "먼저 IPv4 전용 MicroNetwork를 생성한다.")} <a href={viewHash("networks")}>{t("Open networks", "네트워크 열기")}</a></p>}
      <p className="poll-note">{t("A new group has no rules and denies new peer connections. Add allow rules after creating it.", "새 그룹에는 규칙이 없어 새 연결을 차단한다. 생성 후 허용 규칙을 추가한다.")}</p>
      <div className="service-actions">
        <button className="btn primary" type="submit" disabled={saving || eligible.length === 0}>{saving ? t("Creating…", "생성 중…") : t("Create security group", "보안그룹 생성")}</button>
        <button className="btn" type="button" disabled={saving} onClick={onCancel}>{t("Cancel", "취소")}</button>
      </div>
    </form>
  </section>;
}

type RuleDraft = { key: number; rule: SecurityGroupRule };

function RulesForm({ group, networkName, onSaved, onCancel }: {
  group: SecurityGroupResponse; networkName: string; onSaved: (group: SecurityGroupResponse) => void; onCancel: () => void;
}) {
  const { t } = useI18n();
  const [rows, setRows] = useState<RuleDraft[]>(() => group.rules.map((rule, key) => ({ key, rule })));
  const nextKey = useRef(group.rules.length);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const change = (key: number, patch: Partial<SecurityGroupRule>) => {
    setSaved(false);
    setRows((current) => current.map((row) => row.key === key ? { ...row, rule: { ...row.rule, ...patch } } : row));
  };
  const addRule = () => {
    const row: RuleDraft = { key: nextKey.current++, rule: { direction: "ingress", protocol: "tcp", portFrom: 443, portTo: 443, peerCidr: "" } };
    setRows((current) => [...current, row]); setSaved(false);
  };
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (saving) return;
    setSaving(true); setError(null); setSaved(false);
    try {
      const result = await replaceSecurityGroupRules(group.id, { rules: rows.map(({ rule }) => ({ ...rule, peerCidr: rule.peerCidr.trim() })) });
      setRows(result.rules.map((rule) => ({ key: nextKey.current++, rule })));
      onSaved(result); setSaved(true);
    } catch (failure) {
      setError(failure instanceof ApiClientError && failure.status === 409
        ? t("Stop affected VMs and service deployments before changing rules.", "규칙을 변경하려면 연결된 VM과 서비스 배포를 중지하고 정리를 완료한다.")
        : (failure as Error).message);
    } finally { setSaving(false); }
  };

  return <section className="panel" aria-labelledby="security-group-rules-heading" data-editing>
    <h2 className="panel-title" id="security-group-rules-heading">{t(`Rules: ${group.name}`, `규칙: ${group.name}`)}</h2>
    <p className="poll-note">MicroNetwork · {networkName}</p>
    <p className="poll-note">{t("Only listed connections are allowed. Replies remain allowed. Stop affected VMs and services before saving changes.", "목록에 있는 연결만 허용한다. 응답 트래픽은 허용한다. 변경을 저장하기 전에 연결된 VM과 서비스를 중지한다.")}</p>
    {error && <p className="field-error" role="alert">{error}</p>}
    {saved && <p className="poll-note" role="status">{t("Rules saved.", "규칙을 저장했다.")}</p>}
    <form className="security-group-form" onSubmit={submit}>
      {rows.length === 0 && <p className="empty">{t("No allow rules. All new peer connections are denied.", "허용 규칙이 없다. 모든 새 연결을 차단한다.")}</p>}
      {rows.map(({ key, rule }, index) => {
        const id = `security-rule-${key}`;
        return <fieldset key={key} className="security-group-rule" disabled={saving}>
          <legend>{t(`Rule ${index + 1}`, `규칙 ${index + 1}`)}</legend>
          <div className="security-group-rule-fields">
            <div className="field"><label htmlFor={`${id}-direction`}>{t("Direction", "방향")}</label><select id={`${id}-direction`} value={rule.direction} onChange={(event) => change(key, { direction: event.target.value as SecurityGroupRule["direction"] })}><option value="ingress">{t("Inbound", "인바운드")}</option><option value="egress">{t("Outbound", "아웃바운드")}</option></select></div>
            <div className="field"><label htmlFor={`${id}-protocol`}>{t("Protocol", "프로토콜")}</label><select id={`${id}-protocol`} value={rule.protocol} onChange={(event) => change(key, { protocol: event.target.value as SecurityGroupRule["protocol"] })}><option value="tcp">TCP</option><option value="udp">UDP</option></select></div>
            <div className="field"><label htmlFor={`${id}-from`}>{t("Port from", "시작 포트")}</label><input id={`${id}-from`} type="number" min={1} max={65535} value={Number.isNaN(rule.portFrom) ? "" : rule.portFrom} required onChange={(event) => change(key, { portFrom: event.target.valueAsNumber })} /></div>
            <div className="field"><label htmlFor={`${id}-to`}>{t("Port to", "끝 포트")}</label><input id={`${id}-to`} type="number" min={Number.isNaN(rule.portFrom) ? 1 : rule.portFrom} max={65535} value={Number.isNaN(rule.portTo) ? "" : rule.portTo} required onChange={(event) => change(key, { portTo: event.target.valueAsNumber })} /></div>
            <div className="field security-group-peer"><label htmlFor={`${id}-peer`}>{rule.direction === "ingress" ? t("Source IPv4 CIDR", "출발지 IPv4 CIDR") : t("Destination IPv4 CIDR", "목적지 IPv4 CIDR")}</label><input id={`${id}-peer`} value={rule.peerCidr} required maxLength={18} placeholder="172.30.0.0/24" onChange={(event) => change(key, { peerCidr: event.target.value })} /></div>
          </div>
          <button type="button" className="btn danger" aria-label={t(`Remove rule ${index + 1}`, `규칙 ${index + 1} 삭제`)} onClick={() => { setRows((current) => current.filter((row) => row.key !== key)); setSaved(false); }}>{t("Remove rule", "규칙 삭제")}</button>
        </fieldset>;
      })}
      <div className="service-actions">
        <button type="button" className="btn" disabled={saving || rows.length >= 256} onClick={addRule}>{t("Add rule", "규칙 추가")}</button>
        <span className="poll-note">{rows.length} / 256</span>
      </div>
      <div className="service-actions">
        <button className="btn primary" type="submit" disabled={saving}>{saving ? t("Saving…", "저장 중…") : t("Save rules", "규칙 저장")}</button>
        <button className="btn" type="button" disabled={saving} onClick={onCancel}>{t("Close", "닫기")}</button>
      </div>
    </form>
  </section>;
}
