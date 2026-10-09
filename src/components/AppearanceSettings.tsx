import { useRef, useState, type ReactNode } from "react";
import { api } from "../lib/api";
import { useStore } from "../store";
import { THEMES, type Theme } from "../lib/theme";
import type { ThemeChoice, UiPrefs } from "../lib/types";
import { Field, Switch } from "./ui";

/** Settings → Appearance: how the app looks, and how its terminals and agents behave. */
export function AppearanceSettings() {
  const settings = useStore((s) => s.settings);
  const refreshSettings = useStore((s) => s.refreshSettings);
  const fail = useStore((s) => s.fail);

  /**
   * What the sliders say before it is saved.
   *
   * Bound straight to the stored value, a slider only moved once the save
   * came back, so the thumb jumped back between steps of a drag — and every
   * step was a save, rewriting the whole config file.
   */
  const [uiDraft, setUiDraft] = useState<Partial<UiPrefs>>({});
  const uiDraftRef = useRef(uiDraft);
  const uiSaveTimer = useRef<number | undefined>(undefined);
  const uiSaves = useRef(0);
  const ui = settings ? { ...settings.ui, ...uiDraft } : null;

  /** Save a change to the prefs, on top of whatever a slider has not saved yet. */
  async function saveUi(patch: Partial<UiPrefs>) {
    // Read now, not from the render that scheduled this: a slider's save
    // runs after a pause, and anything switched meanwhile must survive it.
    const stored = useStore.getState().settings?.ui;
    if (!stored) return;
    // This save carries the draft, so a slider's pending one is not needed
    // — and firing after it would put the slider's older value back.
    window.clearTimeout(uiSaveTimer.current);
    uiDraftRef.current = { ...uiDraftRef.current, ...patch };
    setUiDraft(uiDraftRef.current);
    const n = ++uiSaves.current;
    try {
      await api.setUiPrefs({ ...stored, ...uiDraftRef.current });
      await refreshSettings();
      if (n === uiSaves.current) {
        uiDraftRef.current = {};
        setUiDraft({});
      }
    } catch (e) {
      fail(e);
    }
  }

  /** A slider: shown at once, saved once it stops moving. */
  function slideUi(patch: Partial<UiPrefs>) {
    // Counts as a newer change, so a save already on its way does not clear
    // the draft when it lands — that threw away a value still waiting on its
    // timer, and the thumb jumped back.
    uiSaves.current += 1;
    uiDraftRef.current = { ...uiDraftRef.current, ...patch };
    setUiDraft(uiDraftRef.current);
    window.clearTimeout(uiSaveTimer.current);
    uiSaveTimer.current = window.setTimeout(() => void saveUi({}), 250);
  }

  if (!settings || !ui) return null;
  return (
    <>
      <Field
        label="Theme"
        hint="Match the Mac switches between Light and Dark as the Mac does. A paired phone draws in the same theme, or under Match the Mac in its own appearance. In a terminal an agent picks its own colours: if one stays dark, change its theme (Claude Code: /theme)."
      >
        <ThemePicker value={ui.theme} onPick={(theme) => void saveUi({ theme })} />
      </Field>

      <Field
        label={`Interface scale — ${Math.round(ui.scale * 100)}%`}
        hint="Scales everything except terminal text, which has its own size below."
      >
        <input
          type="range"
          min={0.8}
          max={1.6}
          step={0.05}
          value={ui.scale}
          onChange={(e) => slideUi({ scale: Number(e.target.value) })}
        />
        <div className="row" style={{ marginTop: 6 }}>
          {[0.9, 1.0, 1.15, 1.3, 1.45].map((v) => (
            <button
              key={v}
              className={`btn btn-sm${
                Math.abs(ui.scale - v) < 0.001 ? " btn-primary" : ""
              }`}
              onClick={() => void saveUi({ scale: v })}
            >
              {Math.round(v * 100)}%
            </button>
          ))}
        </div>
      </Field>

      <Field
        label="Notifications"
        hint="Only while the window is in the background. Opening the app does not announce what was already waiting."
      >
        <div className="switch-list">
          <Switch
            label="New reviews and tickets"
            detail="A banner when a pull request starts waiting on your review, or a ticket is assigned to you."
            checked={settings.ui.system_notifications}
            onChange={(v) => void saveUi({ system_notifications: v })}
          />
          <Switch
            label="Agents waiting on you"
            detail="A banner when an agent goes quiet, asks to trust its folder, runs out of budget or exits — and the number waiting on the dock icon."
            checked={settings.ui.notify_waiting_agents}
            onChange={(v) => void saveUi({ notify_waiting_agents: v })}
          />
        </div>
      </Field>

      <Field
        label="Terminals"
        hint="Agents are resumed rather than restarted where their CLI supports it, so the conversation carries on."
      >
        <div className="switch-list">
          <Switch
            label="Put terminals back when the app reopens"
            detail="Panes that were open last time are reopened in the same worktrees."
            checked={settings.ui.restore_panes}
            onChange={(v) => void saveUi({ restore_panes: v })}
          />
          <Switch
            label="Let agents read terminal output"
            detail="Agents can read what a terminal here has printed — a dev server's log, a test run — instead of starting a second copy. A shell's scrollback is a record of everything typed in it, so this stays off until you want it."
            checked={settings.ui.agents_read_panes}
            onChange={(v) => void saveUi({ agents_read_panes: v })}
          />
          <Switch
            label="Trust the folders this app creates"
            detail="Claude Code asks whether it trusts a folder the first time it starts there, and does nothing until answered — once per task, per repo. This answers it in advance, and only for worktrees and chat folders the app made itself."
            checked={settings.ui.trust_agent_dirs}
            onChange={(v) => void saveUi({ trust_agent_dirs: v })}
          />
        </div>
      </Field>

      <Field
        label="Reviewer"
        hint="The model the Review button in the Diff tab runs on, through Claude Code. Sonnet is the balance; Opus reviews deeper and costs several times as much a run; Haiku is quick and shallow."
      >
        <select
          value={settings.ui.reviewer_model}
          onChange={(e) => void saveUi({ reviewer_model: e.target.value as UiPrefs["reviewer_model"] })}
        >
          <option value="sonnet">Sonnet</option>
          <option value="opus">Opus</option>
          <option value="haiku">Haiku</option>
        </select>
      </Field>

      <TextSize
        label="Terminal text"
        hint="Applies to running panes immediately; they re-fit to the new cell size."
        value={ui.terminal_font_size}
        onChange={(v) => slideUi({ terminal_font_size: v })}
      />
      <TextSize
        label="Conversation text"
        hint="Agents started as a conversation (ACP): what they say, their tool calls and the message box. Applies to open ones immediately."
        value={ui.conversation_font_size}
        onChange={(v) => slideUi({ conversation_font_size: v })}
      />
    </>
  );
}

/**
 * The themes as small pictures of the app, each drawn in its own tokens:
 * three buttons named Dark, Light and Match the Mac said nothing of what a
 * theme looks like, and the names stop saying it once there are more.
 */
function ThemePicker({ value, onPick }: { value: ThemeChoice; onPick: (theme: ThemeChoice) => void }) {
  const tile = (id: ThemeChoice, name: string, shot: ReactNode, note?: string) => (
    <button
      key={id}
      className={`theme-tile${value === id ? " on" : ""}`}
      aria-pressed={value === id}
      onClick={() => onPick(id)}
    >
      <div className="theme-shot">{shot}</div>
      <span className="theme-name">
        {name}
        {note && <span className="theme-note">{note}</span>}
      </span>
    </button>
  );
  return (
    <div className="theme-picker">
      {THEMES.map((t) => tile(t.id, t.name, <Preview theme={t.id} />))}
      {tile(
        "system",
        "Match the Mac",
        <>
          <Preview theme="dark" />
          <Preview theme="light" half />
        </>,
        "Light or Dark",
      )}
    </div>
  );
}

/** The app in miniature: top bar, task list, a page with text, statuses and a button. */
function Preview({ theme, half }: { theme: Theme; half?: boolean }) {
  return (
    <div className={`theme-preview${half ? " left-half" : ""}`} data-theme={theme} aria-hidden>
      <div className="tp-bar"><i className="on" /><i /><i /></div>
      <div className="tp-body">
        <div className="tp-side"><i className="on" /><i /><i style={{ width: "70%" }} /></div>
        <div className="tp-main">
          <i className="text" style={{ width: "80%" }} />
          <i style={{ width: "55%" }} />
          <div className="tp-dots">
            {["--green", "--amber", "--red", "--blue"].map((c) => <b key={c} style={{ background: `var(${c})` }} />)}
          </div>
          <span className="tp-button" />
        </div>
      </div>
    </div>
  );
}

/** A text size in px, 9–24, saved as the slider moves. */
function TextSize({ label, hint, value, onChange }: {
  label: string;
  hint: string;
  value: number;
  onChange: (value: number) => void;
}) {
  return (
    <Field label={`${label} — ${value}px`} hint={hint}>
      <input type="range" min={9} max={24} step={1} value={value} onChange={(e) => onChange(Number(e.target.value))} />
    </Field>
  );
}
