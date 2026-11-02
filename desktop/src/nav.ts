// Back and Forward between the places you have been: a table, a query
// result, the schema board, on this connection or another (backStack and
// forwardStack in Models.swift). A place is recorded once the window has
// settled, so one action that changes several things counts as one step.

import { relationId, type Execution } from "./api";
import type { AppModel } from "./model";
import type { Action } from "./shortcuts";
import { schema } from "./schema/store";

export const LIMIT = 50;

export interface Place {
  /** The saved connection, or null for one that was only pasted. */
  saved: string | null;
  /** The session it was seen in; the only way back to a pasted connection. */
  session: string;
  shown: "table" | "query" | "schema";
  relation?: string;
  /** Which run of the SQL panel, and its result, for a query place. */
  run?: number;
  execution?: Execution | null;
}

/** Where the window is, or null when there is nothing worth coming back to. */
export function placeOf(model: AppModel): Place | null {
  const info = model.session;
  if (!info) return null;
  const base = { saved: info.savedId ?? null, session: info.sessionId };
  switch (model.shown) {
    case "table":
      return model.relation ? { ...base, shown: "table", relation: relationId(model.relation) } : null;
    case "query":
      return model.execution ? { ...base, shown: "query", run: model.runVersion, execution: model.execution } : null;
    case "schema":
      return { ...base, shown: "schema" };
  }
}

export function samePlace(a: Place | null, b: Place | null): boolean {
  if (!a || !b) return a === b;
  return a.saved === b.saved && a.session === b.session && a.shown === b.shown && a.relation === b.relation && a.run === b.run;
}

/** The two stacks and the place between them. Pure: travelling is in `Nav`. */
export class History {
  back: Place[] = [];
  forward: Place[] = [];
  current: Place | null = null;

  /** The window moved on its own: the old place goes back, forward is gone. */
  note(now: Place | null): boolean {
    if (!now || samePlace(now, this.current)) return false;
    if (this.current) {
      this.back.push(this.current);
      if (this.back.length > LIMIT) this.back.shift();
    }
    this.forward = [];
    this.current = now;
    return true;
  }

  /** A streamed run swaps in fuller results under the same run: every
   *  place of that run (here, or already left behind) keeps the latest. */
  refresh(run: number, execution: Execution) {
    for (const place of [this.current, ...this.back, ...this.forward]) {
      if (place?.shown === "query" && place.run === run) place.execution = execution;
    }
  }
}

class Nav {
  private model: AppModel | null = null;
  private history = new History();
  private travelling = false;
  private queued = false;

  get canBack(): boolean {
    return this.history.back.length > 0 && !this.model?.connecting;
  }
  get canForward(): boolean {
    return this.history.forward.length > 0 && !this.model?.connecting;
  }

  /** Starts watching the window. Returns the function that stops. */
  install(model: AppModel): () => void {
    this.model = model;
    this.history = new History();
    const stop = model.subscribe(() => this.settle());
    this.settle();
    return () => {
      stop();
      if (this.model === model) this.model = null;
    };
  }

  private settle() {
    if (this.queued || this.travelling) return;
    this.queued = true;
    // After the change that caused it, so a table and its first page are one step.
    setTimeout(() => {
      this.queued = false;
      const model = this.model;
      if (!model || this.travelling) return;
      if (model.execution) this.history.refresh(model.runVersion, model.execution);
      if (this.history.note(placeOf(model))) model.changed();
    }, 0);
  }

  async goBack() {
    const { back, forward } = this.history;
    while (back.length) {
      const target = back.pop() as Place;
      const here = this.here();
      if (await this.travel(target)) {
        if (here) forward.push(here);
        break;
      }
    }
    this.model?.changed();
  }

  async goForward() {
    const { back, forward } = this.history;
    while (forward.length) {
      const target = forward.pop() as Place;
      const here = this.here();
      if (await this.travel(target)) {
        if (here) back.push(here);
        break;
      }
    }
    this.model?.changed();
  }

  private here(): Place | null {
    return this.model ? placeOf(this.model) : null;
  }

  /** Goes to a place, reconnecting when it belongs to another connection.
   *  False when it no longer exists (a deleted connection or table). */
  private async travel(target: Place): Promise<boolean> {
    const model = this.model;
    if (!model) return false;
    this.travelling = true;
    try {
      if (target.saved !== model.activeSavedId || !model.session || (!target.saved && model.session.sessionId !== target.session)) {
        const saved = target.saved ? model.store.connections.find((c) => c.id === target.saved) : undefined;
        if (!saved) return false;
        await model.connect(saved);
        if (model.activeSavedId !== saved.id || !model.session) return true; // it did not connect: the log says why
        await tablesLoaded(model);
      }
      switch (target.shown) {
        case "table": {
          const relation = model.relations.find((r) => relationId(r) === target.relation);
          if (!relation) return false;
          await model.openRelation(relation);
          return true;
        }
        case "query":
          if (target.execution && target.execution !== model.execution) {
            model.execution = target.execution;
            model.querySort = null;
            model.runVersion++;
          }
          if (!model.execution) return false;
          model.showResultTab("query");
          return true;
        case "schema":
          if (!schema.available(model)) return false;
          await schema.show(model);
          return true;
      }
      return false;
    } finally {
      this.travelling = false;
      this.history.current = placeOf(model) ?? this.history.current;
    }
  }
}

/** A fresh connection reads its tables in the background: wait for them (a few seconds at most). */
async function tablesLoaded(model: AppModel) {
  for (let waited = 0; waited < 8000 && (model.relationsLoading || model.relations.length === 0); waited += 50) {
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
}

export const nav = new Nav();

/** Back and Forward from the menu and keys. Returns true when it took the action. */
export function performNav(action: Action): boolean {
  switch (action) {
    case "goBack":
      if (nav.canBack) void nav.goBack();
      return true;
    case "goForward":
      if (nav.canForward) void nav.goForward();
      return true;
    default:
      return false;
  }
}
