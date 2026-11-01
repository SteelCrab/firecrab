import { useI18n } from "../i18n";
import { PIPE_GROUPS, PIPE_PAGES } from "../lib/pipePages";
import { pipeCardHash, viewHash, type PipeCardId } from "../navigation";
import PipeIcon from "./PipeIcon";
import "../styles/pipe-navigation.css";
export default function PipeNavigation({
  active,
}: {
  active: PipeCardId | null;
}) {
  const { t } = useI18n();
  return (
    <nav className="pipe-subnav" aria-label={t("Pipe navigation", "Pipe 이동")}>
      <a
        className="pipe-subnav-link"
        href={viewHash("pipe")}
        aria-current={active === null ? "page" : undefined}
      >
        <PipeIcon id="overview" />
        <span>{t("Overview", "개요")}</span>
      </a>
      {PIPE_GROUPS.map((group) => (
        <div className="pipe-subnav-group" key={group.label[0]}>
          <div className="pipe-subnav-label">
            {t(group.label[0], group.label[1])}
          </div>
          {group.pages.map((id) => {
            const page = PIPE_PAGES.find((page) => page.id === id)!;
            return (
              <a
                key={id}
                className="pipe-subnav-link"
                href={pipeCardHash(id)}
                aria-current={active === id ? "page" : undefined}
              >
                <PipeIcon id={id} />
                <span>{t(...page.title)}</span>
              </a>
            );
          })}
        </div>
      ))}
    </nav>
  );
}
