import { useState, useRef, useCallback } from "react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Layers, Upload, FileText, X } from "lucide-react";

interface DocumentImportProps {
  onDecompose: (document: string) => Promise<void>;
  decomposing: boolean;
}

export function DocumentImport({ onDecompose, decomposing }: DocumentImportProps) {
  const [docText, setDocText] = useState("");
  const [fileName, setFileName] = useState<string | null>(null);
  const [dragOver, setDragOver] = useState(false);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const handleFile = useCallback((file: File) => {
    if (!file.name.endsWith(".md") && !file.name.endsWith(".txt") && !file.name.endsWith(".markdown")) {
      return;
    }
    const reader = new FileReader();
    reader.onload = (e) => {
      const text = e.target?.result;
      if (typeof text === "string") {
        setDocText(text);
        setFileName(file.name);
      }
    };
    reader.readAsText(file);
  }, []);

  const handleDrop = useCallback(
    (e: React.DragEvent) => {
      e.preventDefault();
      setDragOver(false);
      const file = e.dataTransfer.files[0];
      if (file) handleFile(file);
    },
    [handleFile],
  );

  const handleDragOver = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    setDragOver(true);
  }, []);

  const handleDragLeave = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    setDragOver(false);
  }, []);

  const handleFileInput = useCallback(
    (e: React.ChangeEvent<HTMLInputElement>) => {
      const file = e.target.files?.[0];
      if (file) handleFile(file);
      // Reset so the same file can be selected again
      e.target.value = "";
    },
    [handleFile],
  );

  const handleClear = () => {
    setDocText("");
    setFileName(null);
  };

  const handleSubmit = async () => {
    if (!docText.trim()) return;
    await onDecompose(docText);
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-sm flex items-center gap-2">
          <FileText className="h-4 w-4" />
          Import Design Document
        </CardTitle>
      </CardHeader>
      <CardContent className="space-y-3">
        {/* Drop zone */}
        <div
          onDrop={handleDrop}
          onDragOver={handleDragOver}
          onDragLeave={handleDragLeave}
          className={`relative rounded-md border-2 border-dashed transition-colors ${
            dragOver
              ? "border-blue-500 bg-blue-500/5"
              : "border-border hover:border-muted-foreground/50"
          }`}
        >
          {!docText ? (
            <div className="flex flex-col items-center justify-center gap-2 py-8 text-muted-foreground">
              <Upload className="h-8 w-8" />
              <p className="text-sm">
                Drop a Markdown file here, or{" "}
                <button
                  type="button"
                  className="text-blue-400 hover:underline"
                  onClick={() => fileInputRef.current?.click()}
                >
                  browse
                </button>
              </p>
              <p className="text-xs">Supports .md, .txt, .markdown files</p>
            </div>
          ) : (
            <div className="relative">
              {fileName && (
                <div className="flex items-center justify-between border-b border-border px-3 py-1.5 bg-muted/30">
                  <span className="text-xs text-muted-foreground font-mono truncate">
                    {fileName}
                  </span>
                  <button
                    type="button"
                    onClick={handleClear}
                    className="text-muted-foreground hover:text-foreground p-0.5 rounded"
                  >
                    <X className="h-3 w-3" />
                  </button>
                </div>
              )}
              <textarea
                className="w-full h-48 rounded-md bg-background px-3 py-2 text-sm font-mono resize-y focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring placeholder:text-muted-foreground border-0"
                placeholder="Paste your design document (Markdown) here..."
                value={docText}
                onChange={(e) => {
                  setDocText(e.target.value);
                  if (!e.target.value) setFileName(null);
                }}
              />
            </div>
          )}
          <input
            ref={fileInputRef}
            type="file"
            accept=".md,.txt,.markdown"
            onChange={handleFileInput}
            className="hidden"
          />
        </div>

        {/* Paste area when no file loaded and no text */}
        {!docText && (
          <textarea
            className="w-full h-32 rounded-md border border-input bg-background px-3 py-2 text-sm font-mono resize-y focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring placeholder:text-muted-foreground"
            placeholder="Or paste your design document (Markdown) here..."
            value={docText}
            onChange={(e) => setDocText(e.target.value)}
          />
        )}

        {/* Actions */}
        <div className="flex items-center justify-between">
          <div className="text-xs text-muted-foreground">
            {docText
              ? `${docText.split("\n").length} lines, ${docText.length.toLocaleString()} chars`
              : "No document loaded"}
          </div>
          <div className="flex gap-2">
            {docText && (
              <Button size="sm" variant="ghost" onClick={handleClear}>
                Clear
              </Button>
            )}
            <Button
              size="sm"
              onClick={() => void handleSubmit()}
              disabled={decomposing || !docText.trim()}
            >
              <Layers className="h-4 w-4 mr-1" />
              {decomposing ? "Decomposing..." : "Decompose"}
            </Button>
          </div>
        </div>
      </CardContent>
    </Card>
  );
}
