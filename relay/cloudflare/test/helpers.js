import { DatabaseSync } from "node:sqlite";
import { readFileSync } from "node:fs";

/** A D1-shaped wrapper over node:sqlite, with the real schema. */
export function newDb() {
  const db = new DatabaseSync(":memory:");
  db.exec(readFileSync(new URL("../schema.sql", import.meta.url), "utf8"));
  return {
    prepare(sql) {
      const st = db.prepare(sql);
      let args = [];
      const o = {
        bind(...a) { args = a; return o; },
        async run() { const r = st.run(...args); return { meta: { changes: Number(r.changes) } }; },
        async first() { return st.get(...args) ?? null; },
        async all() { return { results: st.all(...args) }; },
      };
      return o;
    },
    async batch(stmts) { for (const s of stmts) await s.run(); return []; },
    raw: db,
  };
}

export const fixture = () => JSON.parse(readFileSync(new URL("./fixture.json", import.meta.url), "utf8"));
