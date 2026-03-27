import { useState } from "react";
import { RotateCcw } from "lucide-react";
import { useThemeStore, THEME_DEFAULTS, OPACITY_DEFAULTS } from "@/stores/theme";
import { hslToHex, hexToHsl } from "@/lib/color";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

// ---------------------------------------------------------------------------
// Color groups — organized for the settings UI
// ---------------------------------------------------------------------------

interface ColorEntry {
  key: string;
  label: string;
}

interface ColorGroup {
  title: string;
  description: string;
  entries: ColorEntry[];
}

const COLOR_GROUPS: ColorGroup[] = [
  {
    title: "Page",
    description: "Main page background and text",
    entries: [
      { key: "background", label: "Background" },
      { key: "foreground", label: "Text" },
    ],
  },
  {
    title: "Cards",
    description: "Card surfaces and text",
    entries: [
      { key: "card", label: "Background" },
      { key: "card-foreground", label: "Text" },
    ],
  },
  {
    title: "Sidebar",
    description: "Navigation sidebar",
    entries: [
      { key: "sidebar-background", label: "Background" },
      { key: "sidebar-foreground", label: "Text" },
      { key: "sidebar-border", label: "Border" },
      { key: "sidebar-accent", label: "Accent" },
      { key: "sidebar-accent-foreground", label: "Accent text" },
    ],
  },
  {
    title: "Primary",
    description: "Primary action buttons and links",
    entries: [
      { key: "primary", label: "Background" },
      { key: "primary-foreground", label: "Text" },
    ],
  },
  {
    title: "Secondary",
    description: "Secondary actions and badges",
    entries: [
      { key: "secondary", label: "Background" },
      { key: "secondary-foreground", label: "Text" },
    ],
  },
  {
    title: "Accent",
    description: "Hover states and highlights",
    entries: [
      { key: "accent", label: "Background" },
      { key: "accent-foreground", label: "Text" },
    ],
  },
  {
    title: "Muted",
    description: "Subdued backgrounds and secondary text",
    entries: [
      { key: "muted", label: "Background" },
      { key: "muted-foreground", label: "Text" },
    ],
  },
  {
    title: "Destructive",
    description: "Error states and delete actions",
    entries: [
      { key: "destructive", label: "Background" },
      { key: "destructive-foreground", label: "Text" },
    ],
  },
  {
    title: "Borders & Input",
    description: "Borders, input fields, and focus rings",
    entries: [
      { key: "border", label: "Border" },
      { key: "input", label: "Input border" },
      { key: "ring", label: "Focus ring" },
    ],
  },
  {
    title: "Popover",
    description: "Dropdown menus and tooltips",
    entries: [
      { key: "popover", label: "Background" },
      { key: "popover-foreground", label: "Text" },
    ],
  },
];

// ---------------------------------------------------------------------------
// Opacity controls
// ---------------------------------------------------------------------------

interface OpacityEntry {
  key: string;
  label: string;
}

const OPACITY_ENTRIES: OpacityEntry[] = [
  { key: "background", label: "Page background" },
  { key: "card", label: "Cards" },
  { key: "sidebar", label: "Sidebar" },
  { key: "popover", label: "Popovers" },
];

// ---------------------------------------------------------------------------
// Components
// ---------------------------------------------------------------------------

function ColorPicker({
  name,
  label,
  value,
  onChange,
  onReset,
  isOverridden,
}: {
  name: string;
  label: string;
  value: string;
  onChange: (hsl: string) => void;
  onReset: () => void;
  isOverridden: boolean;
}) {
  const hex = hslToHex(value);

  return (
    <div className="flex items-center gap-3">
      <label className="relative cursor-pointer group" title={name}>
        <input
          type="color"
          value={hex}
          onChange={(e) => onChange(hexToHsl(e.target.value))}
          className="absolute inset-0 opacity-0 cursor-pointer w-full h-full"
        />
        <div
          className="w-8 h-8 rounded-md border border-border group-hover:ring-2 ring-ring transition-shadow"
          style={{ backgroundColor: hex }}
        />
      </label>
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="text-sm">{label}</span>
          {isOverridden && (
            <button
              type="button"
              onClick={onReset}
              className="text-xs text-muted-foreground hover:text-foreground transition-colors"
              title="Reset to default"
            >
              <RotateCcw className="h-3 w-3" />
            </button>
          )}
        </div>
        <span className="text-xs text-muted-foreground font-mono">{hex}</span>
      </div>
    </div>
  );
}

function OpacitySlider({
  label,
  value,
  onChange,
}: {
  label: string;
  value: number;
  onChange: (v: number) => void;
}) {
  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between">
        <span className="text-sm">{label}</span>
        <span className="text-xs text-muted-foreground font-mono tabular-nums w-10 text-right">
          {value}%
        </span>
      </div>
      <input
        type="range"
        min={0}
        max={100}
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
        className="w-full h-1.5 rounded-full appearance-none bg-secondary cursor-pointer
          [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:w-3.5
          [&::-webkit-slider-thumb]:h-3.5 [&::-webkit-slider-thumb]:rounded-full
          [&::-webkit-slider-thumb]:bg-primary [&::-webkit-slider-thumb]:cursor-pointer
          [&::-webkit-slider-thumb]:hover:ring-2 [&::-webkit-slider-thumb]:ring-ring
          [&::-moz-range-thumb]:w-3.5 [&::-moz-range-thumb]:h-3.5
          [&::-moz-range-thumb]:rounded-full [&::-moz-range-thumb]:bg-primary
          [&::-moz-range-thumb]:border-0 [&::-moz-range-thumb]:cursor-pointer"
      />
    </div>
  );
}

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

export function Appearance() {
  const { colors, getColor, setColor, resetColor, getOpacity, setOpacity, reset } =
    useThemeStore();

  const overrideCount = Object.keys(colors).length;

  return (
    <div className="p-6 space-y-6 max-w-3xl">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold">Appearance</h1>
          <p className="text-sm text-muted-foreground mt-1">
            Customize colors and transparency. Changes apply instantly.
          </p>
        </div>
        <Button
          size="sm"
          variant="outline"
          onClick={reset}
          disabled={overrideCount === 0}
        >
          <RotateCcw className="h-4 w-4 mr-1" />
          Reset all
          {overrideCount > 0 && (
            <span className="ml-1 text-muted-foreground">
              ({overrideCount})
            </span>
          )}
        </Button>
      </div>

      {/* Transparency controls */}
      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="text-base">Transparency</CardTitle>
          <p className="text-xs text-muted-foreground">
            Control the opacity of background surfaces
          </p>
        </CardHeader>
        <CardContent className="space-y-4">
          {OPACITY_ENTRIES.map((entry) => (
            <OpacitySlider
              key={entry.key}
              label={entry.label}
              value={getOpacity(entry.key)}
              onChange={(v) => setOpacity(entry.key, v)}
            />
          ))}
        </CardContent>
      </Card>

      {/* Color groups */}
      {COLOR_GROUPS.map((group) => (
        <Card key={group.title}>
          <CardHeader className="pb-3">
            <CardTitle className="text-base">{group.title}</CardTitle>
            <p className="text-xs text-muted-foreground">
              {group.description}
            </p>
          </CardHeader>
          <CardContent>
            <div className="grid gap-4 sm:grid-cols-2">
              {group.entries.map((entry) => (
                <ColorPicker
                  key={entry.key}
                  name={entry.key}
                  label={entry.label}
                  value={getColor(entry.key)}
                  onChange={(hsl) => setColor(entry.key, hsl)}
                  onReset={() => resetColor(entry.key)}
                  isOverridden={entry.key in colors}
                />
              ))}
            </div>
          </CardContent>
        </Card>
      ))}

      {/* Import/Export */}
      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="text-base">Theme data</CardTitle>
          <p className="text-xs text-muted-foreground">
            Copy your theme as JSON to share or back up
          </p>
        </CardHeader>
        <CardContent>
          <ThemeExport />
        </CardContent>
      </Card>
    </div>
  );
}

function ThemeExport() {
  const colors = useThemeStore((s) => s.colors);
  const opacity = useThemeStore((s) => s.opacity);
  const hasOverrides =
    Object.keys(colors).length > 0 || Object.keys(opacity).length > 0;

  const exportData = JSON.stringify({ colors, opacity }, null, 2);

  const [importDialogOpen, setImportDialogOpen] = useState(false);
  const [importText, setImportText] = useState("");
  const [importError, setImportError] = useState<string | null>(null);

  const handleCopy = () => {
    navigator.clipboard.writeText(exportData).catch(() => {});
  };

  const handleImport = () => {
    setImportError(null);
    try {
      const data = JSON.parse(importText) as { colors?: Record<string, string>; opacity?: Record<string, number> };
      if (data.colors) {
        for (const [k, v] of Object.entries(data.colors)) {
          if (k in THEME_DEFAULTS) useThemeStore.getState().setColor(k, v);
        }
      }
      if (data.opacity) {
        for (const [k, v] of Object.entries(data.opacity)) {
          if (k in OPACITY_DEFAULTS) useThemeStore.getState().setOpacity(k, v);
        }
      }
      setImportText("");
      setImportDialogOpen(false);
    } catch {
      setImportError("Invalid theme JSON");
    }
  };

  return (
    <div className="space-y-3">
      {hasOverrides ? (
        <pre className="text-xs font-mono bg-muted/30 rounded-md p-3 overflow-x-auto max-h-40">
          {exportData}
        </pre>
      ) : (
        <p className="text-xs text-muted-foreground">
          No customizations yet. Changes you make above will appear here.
        </p>
      )}
      <div className="flex gap-2">
        <Button size="sm" variant="outline" onClick={handleCopy} disabled={!hasOverrides}>
          Copy JSON
        </Button>
        <Button size="sm" variant="outline" onClick={() => { setImportText(""); setImportError(null); setImportDialogOpen(true); }}>
          Import JSON
        </Button>
      </div>

      <Dialog open={importDialogOpen} onOpenChange={setImportDialogOpen}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>Import Theme JSON</DialogTitle>
          </DialogHeader>
          <div className="space-y-2">
            <textarea
              value={importText}
              onChange={(e) => setImportText(e.target.value)}
              placeholder='Paste theme JSON here...'
              rows={8}
              className="flex w-full rounded-md border border-input bg-background px-3 py-2 text-sm font-mono ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 resize-y"
              autoFocus
            />
            {importError && (
              <p className="text-destructive text-xs">{importError}</p>
            )}
          </div>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setImportDialogOpen(false)}>
              Cancel
            </Button>
            <Button onClick={handleImport} disabled={!importText.trim()}>
              Import
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
