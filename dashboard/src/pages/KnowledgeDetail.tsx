import { useEffect, useState } from "react";
import { useParams, Link } from "react-router";
import { ArrowLeft, Edit2, ExternalLink, Save, X } from "lucide-react";
import { knowledge as knowledgeApi } from "@/api/client";
import { Card, CardContent } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { MarkdownRenderer } from "@/components/MarkdownRenderer";
import { formatDateTime, formatRelativeTime } from "@/lib/utils";
import type { KnowledgePage } from "@/lib/types";

export function KnowledgeDetail() {
  const { slug } = useParams<{ slug: string }>();
  const [page, setPage] = useState<KnowledgePage | null>(null);
  const [loading, setLoading] = useState(true);
  const [editing, setEditing] = useState(false);
  const [editContent, setEditContent] = useState("");
  const [editTitle, setEditTitle] = useState("");
  const [editTags, setEditTags] = useState("");
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (!slug) return;
    knowledgeApi
      .get(decodeURIComponent(slug))
      .then(setPage)
      .catch(() => {})
      .finally(() => setLoading(false));
  }, [slug]);

  function startEditing() {
    if (!page) return;
    setEditTitle(page.title);
    setEditContent(page.content);
    setEditTags(page.tags.join(", "));
    setEditing(true);
  }

  function cancelEditing() {
    setEditing(false);
  }

  async function handleSave() {
    if (!page) return;
    setSaving(true);
    try {
      const updated = await knowledgeApi.update(page.slug, {
        title: editTitle.trim() || page.title,
        content: editContent,
        tags: editTags
          .split(",")
          .map((t) => t.trim())
          .filter(Boolean),
      });
      setPage(updated);
      setEditing(false);
    } catch {
      // Error visible in console
    } finally {
      setSaving(false);
    }
  }

  if (loading) return <div className="p-6 text-muted-foreground">Loading…</div>;
  if (!page)
    return (
      <div className="p-6">
        <p className="text-muted-foreground">Page not found.</p>
        <Link to="/knowledge">
          <Button variant="ghost" size="sm" className="mt-2">
            <ArrowLeft className="h-4 w-4 mr-1" /> Back
          </Button>
        </Link>
      </div>
    );

  return (
    <div className="p-6 space-y-4 max-w-3xl">
      {/* Header */}
      <div className="flex items-center gap-3">
        <Link to="/knowledge">
          <Button variant="ghost" size="icon">
            <ArrowLeft className="h-4 w-4" />
          </Button>
        </Link>
        <div className="flex-1 min-w-0">
          {editing ? (
            <Input
              value={editTitle}
              onChange={(e) => setEditTitle(e.target.value)}
              className="text-xl font-bold"
            />
          ) : (
            <>
              <h1 className="text-xl font-bold">{page.title}</h1>
              <p className="font-mono text-xs text-muted-foreground">{page.slug}</p>
            </>
          )}
        </div>
        {editing ? (
          <div className="flex gap-1 shrink-0">
            <Button size="sm" variant="ghost" onClick={cancelEditing} disabled={saving}>
              <X className="h-4 w-4 mr-1" /> Cancel
            </Button>
            <Button size="sm" onClick={() => void handleSave()} disabled={saving}>
              <Save className="h-4 w-4 mr-1" /> {saving ? "Saving…" : "Save"}
            </Button>
          </div>
        ) : (
          <Button size="sm" variant="outline" onClick={startEditing} className="shrink-0">
            <Edit2 className="h-4 w-4 mr-1" /> Edit
          </Button>
        )}
      </div>

      {/* Metadata bar */}
      <div className="flex items-center gap-2 flex-wrap text-xs text-muted-foreground">
        {editing ? (
          <Input
            value={editTags}
            onChange={(e) => setEditTags(e.target.value)}
            placeholder="Tags (comma-separated)"
            className="max-w-xs text-xs h-7"
          />
        ) : (
          page.tags.map((t) => (
            <Link key={t} to={`/knowledge?tag=${encodeURIComponent(t)}`}>
              <Badge variant="secondary" className="cursor-pointer hover:bg-accent">
                {t}
              </Badge>
            </Link>
          ))
        )}
        <span className="text-muted-foreground/60">|</span>
        <span>Created {formatDateTime(page.created)}</span>
        <span className="text-muted-foreground/60">|</span>
        <span>Updated {formatRelativeTime(page.updated)}</span>
      </div>

      {/* Sources */}
      {page.sources.length > 0 && (
        <div className="flex flex-wrap gap-2">
          {page.sources.map((src, idx) => (
            <a
              key={idx}
              href={src.url}
              target="_blank"
              rel="noopener noreferrer"
              className="inline-flex items-center gap-1 text-xs text-blue-400 hover:text-blue-300 bg-muted/30 rounded px-2 py-1"
            >
              <ExternalLink className="h-3 w-3" />
              {src.title || src.url}
            </a>
          ))}
        </div>
      )}

      {/* Contributors */}
      {page.contributors.length > 0 && (
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <span>Contributors:</span>
          {page.contributors.map((c) => (
            <Badge key={c} variant="outline" className="text-xs font-mono">
              {c}
            </Badge>
          ))}
        </div>
      )}

      {/* Content */}
      <Card>
        <CardContent className="pt-4">
          {editing ? (
            <textarea
              value={editContent}
              onChange={(e) => setEditContent(e.target.value)}
              rows={20}
              className="flex w-full rounded-md border border-input bg-background px-3 py-2 text-sm font-mono ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 resize-y leading-relaxed"
            />
          ) : (
            <MarkdownRenderer content={page.content} />
          )}
        </CardContent>
      </Card>
    </div>
  );
}
