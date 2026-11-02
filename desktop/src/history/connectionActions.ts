// Open in Terminal and Copy URL, for a saved connection: from the menu bar
// (the open connection) and from the sidebar's context menu (any of them).

import { api, errorText, type SavedConnection } from "../api";
import { copyText } from "../format";
import { t } from "../i18n";
import type { AppModel } from "../model";

/** The password stays on the clipboard this long. */
const SECRET_MS = 60_000;

function openSaved(model: AppModel): SavedConnection | undefined {
  return model.store.connections.find((c) => c.id === model.activeSavedId);
}

/** Starts psql, mysql, sqlite3, redis-cli or mongosh for the connection in a terminal. */
export async function openInTerminal(model: AppModel, saved: SavedConnection | undefined = openSaved(model)) {
  if (!saved) {
    model.showToast(t("Save this connection first."));
    return;
  }
  const session = model.session && model.activeSavedId === saved.id ? model.session : null;
  if (saved.ssh && !session) {
    model.showToast(t("This connection goes through SSH. Connect to it in dbLens, then open the terminal: it uses the same tunnel."));
    return;
  }
  try {
    const result = await api.openInTerminal(saved.id, session?.sessionId ?? null);
    if (result.missing) {
      const install = result.install ?? "";
      model.open({
        kind: "confirm",
        title: t("%@ isn't installed", result.missing),
        message: t("Install it, then try again: %@", install),
        action: t("Copy Command"),
        destructive: false,
        run: () => copyText(install).then(() => model.showToast(t("Copied"))),
      });
      return;
    }
    model.showToast(t("Opened %@ in the terminal", result.tool ?? ""));
  } catch (e) {
    model.showToast(errorText(e));
  }
}

/** The URL with its passwords hidden. */
export async function copyUrl(model: AppModel, saved: SavedConnection | undefined = openSaved(model)) {
  if (!saved) {
    model.showToast(t("Save this connection first."));
    return;
  }
  try {
    await copyText(await api.connectionUrl(saved.id));
    model.showToast(t("Copied"));
  } catch (e) {
    model.showToast(errorText(e));
  }
}

/** The URL as saved, after asking: the password goes in plain text to the clipboard. */
export function copyUrlWithPassword(model: AppModel, saved: SavedConnection | undefined = openSaved(model)) {
  if (!saved) {
    model.showToast(t("Save this connection first."));
    return;
  }
  model.open({
    kind: "confirm",
    title: t("Copy the URL with its password?"),
    message: t("The password will be on the clipboard in plain text. dbLens clears it after a minute."),
    action: t("Copy"),
    destructive: false,
    run: async () => {
      try {
        const secret = await api.connectionUrlWithPassword(saved.id);
        await copyText(secret);
        model.showToast(t("Copied. Cleared from the clipboard after a minute."));
        setTimeout(() => void clearIfUnchanged(secret), SECRET_MS);
      } catch (e) {
        model.showToast(errorText(e));
      }
    },
  });
}

/** Wipes the clipboard, unless the person copied something else since. */
async function clearIfUnchanged(secret: string) {
  try {
    if ((await navigator.clipboard.readText()) === secret) await copyText("");
  } catch {
    // The clipboard can't be read here: leave it rather than wipe something else.
  }
}

/** "Let Agents Write" on the open connection (the MCP server may run changes there), or off again. */
export async function toggleAgentsWrite(model: AppModel, saved: SavedConnection | undefined = openSaved(model)) {
  if (!saved) {
    model.showToast(t("Save this connection first."));
    return;
  }
  const allowed = !saved.agentsCanWrite;
  await model.setAgentsCanWrite(saved, allowed);
  model.showToast(allowed ? t("Agents can now change %@", saved.name) : t("Agents can only read %@ now", saved.name));
}
