import { get } from "../db";
import { insert } from "./store";

// Preview seed — a small central-kitchen dataset (eatdis-flavoured) so the
// dashboard looks alive inside the Clawnify iframe before the owner enters
// anything. Runs once when the suppliers table is empty; the moment real data
// exists it never touches the DB again.
//
// Base units are PURCHASE units (kg / L / unit) so integer cents-per-unit stay
// meaningful (per-gram costs would round to zero). Recipe quantities are
// fractional amounts of that unit.

let seeded = false;

const dayISO = (daysAgo: number) =>
  new Date(Date.now() - daysAgo * 86_400_000).toISOString().slice(0, 10);

export async function ensureSeed(): Promise<void> {
  if (seeded) return;
  try {
    const c = await get<{ n: number }>("SELECT COUNT(*) AS n FROM suppliers");
    if ((c?.n ?? 0) > 0) {
      seeded = true;
      return;
    }
    await runSeed();
    seeded = true;
  } catch (error) {
    // Let the local adapter roll back a failed seed instead of persisting a
    // partially initialized database.
    throw error;
  }
}

async function runSeed(): Promise<void> {
  const bidfood = await insert("suppliers", {
    name: "Bidfood",
    contact_name: "Sales desk",
    email: "orders@bidfood.nl",
    phone: "+31 20 123 4567",
  });
  const groente = await insert("suppliers", {
    name: "Amsterdam Groente",
    contact_name: "Karim",
    email: "hallo@amsterdamgroente.nl",
  });

  const ing = async (
    name: string,
    unit: string,
    cost: number,
    supplier: string,
    par: number,
    allergens?: string,
    category?: string,
  ) =>
    insert("ingredients", {
      name,
      unit,
      cost_cents: cost,
      supplier_id: supplier,
      par_level: par,
      allergens,
      category,
    });

  const flour = await ing("Flour (type 00)", "kg", 150, bidfood, 10, "gluten", "dry");
  const oil = await ing("Olive oil", "L", 1150, bidfood, 5, undefined, "dry");
  const chicken = await ing("Chicken breast", "kg", 1180, bidfood, 8, undefined, "meat");
  const chickpeas = await ing("Chickpeas", "kg", 260, bidfood, 6, undefined, "dry");
  const tomatoes = await ing("Tomatoes", "kg", 320, groente, 12, undefined, "produce");
  const cucumber = await ing("Cucumber", "kg", 220, groente, 8, undefined, "produce");
  const tahini = await ing("Tahini", "kg", 780, bidfood, 3, "sesame", "dry");
  const lemon = await ing("Lemon", "unit", 35, groente, 40, undefined, "produce");
  const yeast = await ing("Fresh yeast", "kg", 1100, bidfood, 1, undefined, "dry");
  const salt = await ing("Sea salt", "kg", 110, bidfood, 2, undefined, "dry");

  const item = (
    recipe: string,
    kind: "ingredient" | "recipe",
    ref: string,
    qty: number,
    unit: string,
  ) =>
    insert("recipe_items", {
      recipe_id: recipe,
      item_type: kind,
      ingredient_id: kind === "ingredient" ? ref : undefined,
      sub_recipe_id: kind === "recipe" ? ref : undefined,
      qty,
      unit,
    });

  // Prep (sub-recipe): hummus, yields 5 kg.
  const hummus = await insert("recipes", {
    name: "Hummus",
    kind: "prep",
    category: "sauce",
    yield_qty: 5,
    yield_unit: "kg",
  });
  await item(hummus, "ingredient", chickpeas, 3, "kg");
  await item(hummus, "ingredient", tahini, 0.8, "kg");
  await item(hummus, "ingredient", oil, 0.3, "L");
  await item(hummus, "ingredient", lemon, 6, "unit");
  await item(hummus, "ingredient", salt, 0.05, "kg");

  // Dish: Mezze bowl, 10 portions @ €9.95, target 30%.
  const mezze = await insert("recipes", {
    name: "Mezze bowl",
    kind: "dish",
    category: "main",
    yield_qty: 10,
    yield_unit: "portion",
    menu_price_cents: 995,
    target_food_cost_pct: 30,
  });
  await item(mezze, "recipe", hummus, 1.5, "kg");
  await item(mezze, "ingredient", cucumber, 1.2, "kg");
  await item(mezze, "ingredient", tomatoes, 1.0, "kg");
  await item(mezze, "ingredient", chickpeas, 0.5, "kg");
  await item(mezze, "ingredient", oil, 0.15, "L");
  await item(mezze, "ingredient", salt, 0.03, "kg");

  // Dish: Focaccia, 20 portions @ €3.50, target 28%.
  const focaccia = await insert("recipes", {
    name: "Focaccia",
    kind: "dish",
    category: "side",
    yield_qty: 20,
    yield_unit: "portion",
    menu_price_cents: 350,
    target_food_cost_pct: 28,
  });
  await item(focaccia, "ingredient", flour, 3, "kg");
  await item(focaccia, "ingredient", oil, 0.4, "L");
  await item(focaccia, "ingredient", yeast, 0.06, "kg");
  await item(focaccia, "ingredient", salt, 0.06, "kg");

  // Dish: Chicken shawarma bowl, 10 portions @ €11.50, target 32%.
  const shawarma = await insert("recipes", {
    name: "Chicken shawarma bowl",
    kind: "dish",
    category: "main",
    yield_qty: 10,
    yield_unit: "portion",
    menu_price_cents: 1150,
    target_food_cost_pct: 32,
  });
  await item(shawarma, "ingredient", chicken, 2.5, "kg");
  await item(shawarma, "ingredient", tomatoes, 0.8, "kg");
  await item(shawarma, "ingredient", cucumber, 0.6, "kg");
  await item(shawarma, "ingredient", oil, 0.2, "L");
  await item(shawarma, "ingredient", lemon, 4, "unit");
  await item(shawarma, "ingredient", salt, 0.04, "kg");

  // Production over the last week (drives theoretical usage).
  const run = (recipe: string, qty: number, daysAgo: number) =>
    insert("production_runs", {
      recipe_id: recipe,
      qty,
      run_date: dayISO(daysAgo),
      source: "manual",
    });
  await run(mezze, 120, 6);
  await run(focaccia, 200, 6);
  await run(shawarma, 90, 5);
  await run(mezze, 140, 2);
  await run(focaccia, 220, 2);

  // Latest stock counts — a couple deliberately below par.
  const count = (ingredient: string, qty: number) =>
    insert("inventory_counts", {
      ingredient_id: ingredient,
      counted_qty: qty,
      count_date: dayISO(1),
      counted_by: "Kitchen",
    });
  await count(flour, 14);
  await count(oil, 7);
  await count(chicken, 9);
  await count(chickpeas, 4); // below par 6
  await count(tomatoes, 15);
  await count(cucumber, 10);
  await count(tahini, 2); // below par 3
  await count(lemon, 55);
  await count(yeast, 1.5);
  await count(salt, 3);

  // Price history so the dashboard can show cost drift.
  const price = (
    ingredient: string,
    supplier: string,
    cents: number,
    daysAgo: number,
  ) =>
    insert("supplier_price_history", {
      ingredient_id: ingredient,
      supplier_id: supplier,
      unit_cost_cents: cents,
      effective_date: dayISO(daysAgo),
    });
  await price(flour, bidfood, 130, 45);
  await price(flour, bidfood, 150, 3); // +15.4%
  await price(chicken, bidfood, 1050, 40);
  await price(chicken, bidfood, 1180, 4); // +12.4%
  await price(tahini, bidfood, 700, 38);
  await price(tahini, bidfood, 780, 6); // +11.4%

  // One pending supplier invoice awaiting reconciliation.
  const invoice = await insert("invoices", {
    supplier_id: bidfood,
    invoice_number: "BF-2026-8841",
    invoice_date: dayISO(3),
    total_cents: 25_980,
    status: "pending",
    source: "agent-ocr",
  });
  await insert("invoice_lines", {
    invoice_id: invoice,
    ingredient_id: flour,
    description: "Flour type 00 — 25kg sack ×4",
    qty: 100,
    unit: "kg",
    unit_cost_cents: 150,
    line_total_cents: 15_000,
  });
  await insert("invoice_lines", {
    invoice_id: invoice,
    ingredient_id: chicken,
    description: "Chicken breast — 6kg",
    qty: 6,
    unit: "kg",
    unit_cost_cents: 1180,
    line_total_cents: 7_080,
  });
  await insert("invoice_lines", {
    invoice_id: invoice,
    ingredient_id: tahini,
    description: "Tahini — 5kg tub",
    qty: 5,
    unit: "kg",
    unit_cost_cents: 780,
    line_total_cents: 3_900,
  });

  // Compliance documents — a lifecycle the agent watches.
  await insert("documents", {
    kind: "haccp",
    name: "HACCP certification",
    issuer: "NVWA-audited",
    reference: "HACCP-2025-AMS-0417",
    issued_date: dayISO(335),
    expiry_date: dayISO(-28), // ~28 days out
    status: "expiring",
  });
  await insert("documents", {
    kind: "license",
    name: "Food business registration",
    issuer: "Gemeente Amsterdam",
    reference: "EXP-AMS-118822",
    issued_date: dayISO(400),
    expiry_date: dayISO(-210),
    status: "valid",
  });
  await insert("documents", {
    kind: "lease",
    name: "Kitchen lease — Haarlemmerplein 37",
    issuer: "Property NL BV",
    issued_date: dayISO(700),
    expiry_date: dayISO(-520),
    status: "valid",
  });
}
