/**
 * Which of `theme.css`'s palettes the page draws in (SET-5): the app's and
 * the phone's. The choice is saved with the settings; "system" follows the
 * appearance of the machine showing the page, as it changes.
 */
import { useEffect, useState } from "react";
import { readOneOf, write } from "./persist";
import type { ThemeChoice } from "./types";

export type Theme = "dark" | "light";

const CHOICES: readonly ThemeChoice[] = ["dark", "light", "system"];
const systemLight = window.matchMedia("(prefers-color-scheme: light)");

function resolve(choice: ThemeChoice, light = systemLight.matches): Theme {
  return choice === "system" ? (light ? "light" : "dark") : choice;
}

/**
 * The choice drawn last time, kept on this machine. The settings arrive
 * after the first paint, and a light app opened dark until they did.
 */
function remembered(): ThemeChoice {
  return readOneOf("theme", CHOICES, "dark");
}

/** Before the first render: the theme the page was last drawn in. */
export function paintRemembered() {
  document.documentElement.dataset.theme = resolve(remembered());
}

/** The theme the page is drawn in now, for what draws itself (xterm). */
export function currentTheme(): Theme {
  return document.documentElement.dataset.theme === "light" ? "light" : "dark";
}

/**
 * Draw the page in the theme `choice` names, and keep following the system
 * while it is "system". Until the choice is known, the one drawn last time.
 */
export function useTheme(choice: ThemeChoice | undefined): Theme {
  const known = choice !== undefined && CHOICES.includes(choice);
  const wanted = known ? choice : remembered();
  const [light, setLight] = useState(systemLight.matches);
  useEffect(() => {
    if (wanted !== "system") return;
    const follow = () => setLight(systemLight.matches);
    // It may have changed while nothing was listening.
    follow();
    systemLight.addEventListener("change", follow);
    return () => systemLight.removeEventListener("change", follow);
  }, [wanted]);
  const theme = resolve(wanted, light);
  useEffect(() => {
    document.documentElement.dataset.theme = theme;
  }, [theme]);
  useEffect(() => {
    if (known) write("theme", wanted);
  }, [known, wanted]);
  return theme;
}
