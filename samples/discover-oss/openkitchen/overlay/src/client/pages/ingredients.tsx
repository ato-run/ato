import { useState, type FormEvent } from "react";
import { api, money, qty, useApi } from "../lib";
import { Card, Chip, Empty, Toolbar, Zone } from "../ui";

interface Ingredient {
  id: string;
  name: string;
  category: string | null;
  unit: string;
  cost_cents: number;
  par_level: number | null;
  allergens: string | null;
}

export function IngredientsPage() {
  const { data, loading, reload } = useApi<Ingredient[]>("/api/ingredients");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  async function addIngredient(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const form = event.currentTarget;
    const values = new FormData(form);
    const name = String(values.get("name") ?? "").trim();
    const unit = String(values.get("unit") ?? "").trim();
    const cents = Number(values.get("cents"));
    if (!name || !unit || !Number.isSafeInteger(cents) || cents < 0) {
      setError("Enter a name, unit, and non-negative whole number of cents.");
      return;
    }
    setBusy(true); setError("");
    try {
      await api("/api/ingredients", { method: "POST", body: JSON.stringify({name, unit, cost_cents:cents}) });
      form.reset(); reload();
    } catch (err) { setError(`Could not save ingredient: ${String(err)}`); }
    finally { setBusy(false); }
  }
  const rows = data ?? [];
  return (
    <>
      <Toolbar title="Ingredients" subtitle="Cost per unit updates when invoices are reconciled" />
      <div className="mx-auto max-w-[75rem] space-y-6 p-6">
        <Card>
          <Zone eyebrow="Add ingredient">
            <form onSubmit={addIngredient} className="flex flex-wrap items-end gap-3">
              <label>Name <input name="name" required maxLength={120} className="border p-1" /></label>
              <label>Unit <input name="unit" required maxLength={20} defaultValue="kg" className="border p-1" /></label>
              <label>Cost per unit (cents) <input name="cents" type="number" min="0" step="1" required className="border p-1" /></label>
              <button type="submit" disabled={busy} className="border rounded px-3 py-1">{busy ? "Saving…" : "Add ingredient"}</button>
            </form>
            {error && <p role="alert">{error}</p>}
          </Zone>
        </Card>
        <Card>
          <Zone eyebrow={`Ingredients · ${rows.length}`}>
            {loading ? (
              <p className="text-sm text-muted">Loading…</p>
            ) : rows.length === 0 ? (
              <Empty>No ingredients yet. Your agent adds these as invoices come in.</Empty>
            ) : (
              <table className="w-full text-sm">
                <thead>
                  <tr className="text-left text-xs font-semibold uppercase tracking-wide text-muted">
                    <th className="py-2">Name</th>
                    <th className="py-2">Category</th>
                    <th className="py-2 text-right">Cost / unit</th>
                    <th className="py-2 text-right">Par</th>
                    <th className="py-2">Allergens</th>
                  </tr>
                </thead>
                <tbody>
                  {rows.map((r) => (
                    <tr key={r.id} className="border-t border-border">
                      <td className="py-2 text-foreground">{r.name}</td>
                      <td className="py-2">{r.category && <Chip>{r.category}</Chip>}</td>
                      <td className="tnum py-2 text-right">
                        {money(r.cost_cents)}
                        <span className="text-muted"> /{r.unit}</span>
                      </td>
                      <td className="tnum py-2 text-right text-muted">
                        {qty(r.par_level, r.unit)}
                      </td>
                      <td className="py-2">
                        <span className="flex flex-wrap gap-1">
                          {(r.allergens ?? "")
                            .split(",")
                            .map((a) => a.trim())
                            .filter(Boolean)
                            .map((a) => (
                              <Chip key={a}>{a}</Chip>
                            ))}
                        </span>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </Zone>
        </Card>
      </div>
    </>
  );
}
