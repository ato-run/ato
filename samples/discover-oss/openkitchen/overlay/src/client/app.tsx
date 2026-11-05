import { useEffect, useState, type ComponentType } from "react";
import {
  LayoutDashboard,
  BookOpen,
  Wheat,
  Truck,
  Boxes,
  ChefHat,
  ReceiptText,
  ShieldCheck,
  type LucideIcon,
} from "lucide-react";
import { navigate, usePath } from "./router";
import { DashboardPage } from "./pages/dashboard";
import { RecipesPage, RecipeDetailPage } from "./pages/recipes";
import { IngredientsPage } from "./pages/ingredients";
import { InvoicesPage, InvoiceDetailPage } from "./pages/invoices";
import { DocumentsPage } from "./pages/documents";
import { SuppliersPage, InventoryPage, ProductionPage } from "./pages/lists";

interface NavItem {
  to: string;
  label: string;
  icon: LucideIcon;
}
const GROUPS: { title: string; items: NavItem[] }[] = [
  { title: "Overview", items: [{ to: "/", label: "Dashboard", icon: LayoutDashboard }] },
  {
    title: "Menu",
    items: [
      { to: "/recipes", label: "Recipes", icon: BookOpen },
      { to: "/ingredients", label: "Ingredients", icon: Wheat },
      { to: "/suppliers", label: "Suppliers", icon: Truck },
    ],
  },
  {
    title: "Operations",
    items: [
      { to: "/inventory", label: "Inventory", icon: Boxes },
      { to: "/production", label: "Production", icon: ChefHat },
      { to: "/invoices", label: "Invoices", icon: ReceiptText },
    ],
  },
  {
    title: "Compliance",
    items: [{ to: "/documents", label: "Documents", icon: ShieldCheck }],
  },
];

export function App() {
  const path = usePath();
  const [storageError, setStorageError] = useState("");
  useEffect(() => {
    const onError = (event: Event) => setStorageError((event as CustomEvent<string>).detail);
    window.addEventListener("openkitchen-storage-error", onError);
    return () => window.removeEventListener("openkitchen-storage-error", onError);
  }, []);
  return (
    <div className="flex min-h-screen">
      <aside className="hidden w-[260px] shrink-0 flex-col border-r border-border bg-surface md:flex">
        <div className="flex items-center gap-2 px-5 py-4">
          <ChefHat className="size-5 text-primary" />
          <span className="text-sm font-bold text-foreground">OpenKitchen</span>
        </div>
        <nav className="flex-1 space-y-5 px-3 py-2">
          {GROUPS.map((g) => (
            <div key={g.title}>
              <div className="px-2 pb-1 text-[0.6875rem] font-semibold uppercase tracking-[0.08em] text-faint">
                {g.title}
              </div>
              {g.items.map((it) => {
                const active =
                  it.to === "/" ? path === "/" : path.startsWith(it.to);
                const Icon = it.icon;
                return (
                  <a
                    key={it.to}
                    href={`#${it.to}`}
                    onClick={(e) => {
                      if (e.metaKey || e.ctrlKey || e.button !== 0) return;
                      e.preventDefault();
                      navigate(it.to);
                    }}
                    className={`flex items-center gap-2 rounded-md px-2.5 py-[0.4375rem] text-sm ${
                      active
                        ? "bg-primary/12 font-semibold text-primary"
                        : "text-foreground hover:bg-sunken"
                    }`}
                  >
                    <Icon className="size-4 shrink-0" />
                    {it.label}
                  </a>
                );
              })}
            </div>
          ))}
        </nav>
      </aside>
      <main className="min-w-0 flex-1">
        <div role="note" style={{padding:"12px", background:"#fff3c4", color:"#3c3000"}}>Local preview: data stays in this browser on this device. It is not shared or backed up and can be lost if browser storage is cleared. Do not use real business data. <a href={'/' + 'source.html'}>Source code and licenses</a>.</div>
        {storageError && <div role="alert" style={{padding:"12px", background:"#ffd4d4", color:"#500"}}>Storage unavailable: {storageError} Reload the page before trying again.</div>}
        <Route path={path} />
      </main>
    </div>
  );
}

function Route({ path }: { path: string }) {
  const recipe = path.match(/^\/recipes\/(.+)$/);
  if (recipe) return <RecipeDetailPage id={recipe[1]} />;
  const invoice = path.match(/^\/invoices\/(.+)$/);
  if (invoice) return <InvoiceDetailPage id={invoice[1]} />;
  const routes: Record<string, ComponentType> = {
    "/": DashboardPage,
    "/recipes": RecipesPage,
    "/ingredients": IngredientsPage,
    "/suppliers": SuppliersPage,
    "/inventory": InventoryPage,
    "/production": ProductionPage,
    "/invoices": InvoicesPage,
    "/documents": DocumentsPage,
  };
  const Page = routes[path];
  return Page ? <Page /> : <DashboardPage />;
}
