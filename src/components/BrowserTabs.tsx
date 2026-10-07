import type { TabInfo } from "../lib/types";
import { CloseIcon, PlusIcon } from "./icons";
import { Spinner } from "./ui";

/**
 * A task's browser tabs (BRW-16): one active, shared with its agents, named
 * by their page's title (BRW-17). Does nothing while an agent is using the
 * browser (BRW-15): the user takes it over first.
 */
export function BrowserTabs({
  tabs,
  locked,
  onSwitch,
  onClose,
  onNew,
}: {
  tabs: TabInfo[];
  locked: boolean;
  onSwitch: (id: string) => void;
  onClose: (id: string) => void;
  onNew: () => void;
}) {
  return (
    <div className={`browser-tabs${locked ? " locked" : ""}`}>
      {tabs.map((t) => (
        <div
          key={t.id}
          className={`browser-tab${t.active ? " active" : ""}`}
          title={`${t.name}\n${t.url}`}
          onClick={() => { if (!locked && !t.active) onSwitch(t.id); }}
          onAuxClick={(e) => { if (!locked && e.button === 1) onClose(t.id); }}
        >
          {t.loading && <Spinner />}
          <span className="name">{t.name}</span>
          <button
            className="x"
            title="Close this tab"
            disabled={locked}
            onClick={(e) => { e.stopPropagation(); onClose(t.id); }}
          >
            <CloseIcon size={12} />
          </button>
        </div>
      ))}
      <button className="btn btn-sm btn-icon browser-tab-new" title="New tab" disabled={locked} onClick={onNew}>
        <PlusIcon size={13} />
      </button>
    </div>
  );
}
