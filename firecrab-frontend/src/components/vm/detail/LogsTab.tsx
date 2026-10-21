import { useEffect, useRef } from "react";
import type { VmResponse } from "../../../bindings";
import { useI18n } from "../../../i18n";
import { logDownloadFilename } from "../../../lib/textExport";
import LogExportActions from "../../LogExportActions";
import { DetailSection } from "../../ui/DetailSection";
import type { VmDetailData } from "./useVmDetail";

/** Start pipeline lines, then the captured guest console output. */
export default function LogsTab({ vm, detail }: { vm: VmResponse; detail: VmDetailData }) {
  const { t } = useI18n();
  const logRef = useRef<HTMLPreElement>(null);
  const text = [...detail.pipelineLines, detail.consoleLog].filter(Boolean).join("\n");

  useEffect(() => {
    const element = logRef.current;
    if (element) element.scrollTop = element.scrollHeight;
  }, [text]);

  return (
    <DetailSection
      title={t("Startup · console log", "시작 · 콘솔 로그")}
      aside={<LogExportActions text={text} filename={logDownloadFilename("vm-log", vm.name)} buttonClassName="btn sm ghost" />}
    >
      <pre className="detail-log" ref={logRef}>{text || t("No output yet.", "아직 출력이 없습니다.")}</pre>
    </DetailSection>
  );
}
