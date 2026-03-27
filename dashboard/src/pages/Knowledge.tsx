import { useEffect, useState } from "react";
import { Link } from "react-router";
import { Plus, Search, BookOpen } from "lucide-react";
import { knowledge as knowledgeApi } from "@/api/client";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { formatRelativeTime } from "@/lib/utils";
import type { KnowledgePage, KnowledgeSearchMatch } from "@/lib/types";

export function Knowledge() {
  const [pages, setPages] = useState<KnowledgePage[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const [activeTag, setActiveTag] = useState<string | null>(null);
  const [searchResults, setSearchResults] = useState<KnowledgeSearchMatch[] | null>(null);
  const [searching, setSearching] = useState(false);
  const [showCreate, setShowCreate] = useState(false);

  useEffect(() => {
    knowledgeApi
      .list()
      .then(setPages)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, []);

  // Debounced API search
  useEffect(() => {
    if (search.length < 2) {
      setSearchResults(null);
      return;
    }
    const timer = setTimeout(() => {
      setSearching(true);
      knowledgeApi
        .search(search)
        .then(setSearchResults)
        .catch(() => setSearchResults(null))
        .finally(() => setSearching(false));
    }, 300);
    return () => clearTimeout(timer);
  }, [search]);

  // Collect all unique tags
  const allTags = Array.from(new Set(pages.flatMap((p) => p.tags))).sort();

  // Client-side filter by tag (search results come from API)
  const filtered = searchResults
    ? pages.filter((p) => searchResults.some((r) => r.slug === p.slug))
    : pages.filter(
        (p) => !activeTag || p.tags.includes(activeTag),
      );

  const refetch = () => {
    knowledgeApi.list().then(setPages).catch((e) => setError(String(e)));
  };

  return (
    <div className="p-6 space-y-4">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">Knowledge</h1>
        <Button size="sm" onClick={() => setShowCreate(true)}>
          <Plus className="h-4 w-4 mr-1" /> New Page
        </Button>
      </div>

      {/* Search + tag filters */}
      <div className="flex flex-wrap items-center gap-3">
        <div className="relative max-w-xs flex-1">
          <Search className="absolute left-2.5 top-2.5 h-4 w-4 text-muted-foreground" />
          <Input
            placeholder="Search knowledge…"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            className="pl-9"
          />
        </div>
        {allTags.length > 0 && (
          <div className="flex flex-wrap gap-1">
            <Button
              size="sm"
              variant={activeTag === null ? "secondary" : "ghost"}
              onClick={() => setActiveTag(null)}
              className="h-7 text-xs"
            >
              All
            </Button>
            {allTags.map((tag) => (
              <Button
                key={tag}
                size="sm"
                variant={activeTag === tag ? "secondary" : "ghost"}
                onClick={() => setActiveTag(activeTag === tag ? null : tag)}
                className="h-7 text-xs"
              >
                {tag}
              </Button>
            ))}
          </div>
        )}
      </div>

      {/* Search result context lines */}
      {searchResults && search.length >= 2 && (
        <p className="text-xs text-muted-foreground">
          {searching ? "Searching…" : `${searchResults.length} result${searchResults.length !== 1 ? "s" : ""} for "${search}"`}
        </p>
      )}

      {error && (
        <p className="text-destructive text-sm">{error}</p>
      )}

      {loading ? (
        <p className="text-muted-foreground text-sm">Loading…</p>
      ) : filtered.length === 0 ? (
        <Card>
          <CardContent className="py-10 text-center">
            <BookOpen className="h-8 w-8 text-muted-foreground/40 mx-auto mb-3" />
            <p className="text-muted-foreground text-sm">
              {search ? "No pages match your search." : "No knowledge pages yet."}
            </p>
            {!search && (
              <p className="text-xs text-muted-foreground mt-1">
                Create one with the button above, or run{" "}
                <code className="bg-muted px-1 rounded text-xs">crosslink knowledge add</code>
              </p>
            )}
          </CardContent>
        </Card>
      ) : (
        <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
          {filtered.map((page) => {
            const matchContext = searchResults?.find((r) => r.slug === page.slug);
            return (
              <Link key={page.slug} to={`/knowledge/${encodeURIComponent(page.slug)}`}>
                <Card className="hover:bg-accent/30 transition-colors cursor-pointer h-full">
                  <CardHeader className="pb-2">
                    <CardTitle className="text-sm">{page.title}</CardTitle>
                    <p className="text-xs text-muted-foreground font-mono">{page.slug}</p>
                  </CardHeader>
                  <CardContent className="space-y-2">
                    <div className="flex flex-wrap gap-1">
                      {page.tags.map((t) => (
                        <Badge key={t} variant="secondary" className="text-xs">
                          {t}
                        </Badge>
                      ))}
                    </div>
                    {matchContext && matchContext.context_lines.length > 0 && (
                      <p className="text-xs text-muted-foreground font-mono line-clamp-2 bg-muted/30 rounded px-2 py-1">
                        {matchContext.context_lines[0][1]}
                      </p>
                    )}
                    <div className="flex items-center justify-between text-xs text-muted-foreground">
                      <span>Updated {formatRelativeTime(page.updated)}</span>
                      {page.contributors.length > 0 && (
                        <span>{page.contributors.length} contributor{page.contributors.length !== 1 ? "s" : ""}</span>
                      )}
                    </div>
                  </CardContent>
                </Card>
              </Link>
            );
          })}
        </div>
      )}

      {/* Create knowledge page dialog */}
      <CreateKnowledgePageDialog
        open={showCreate}
        onOpenChange={setShowCreate}
        onCreated={refetch}
      />
    </div>
  );
}

// ---------------------------------------------------------------------------
// Create Knowledge Page Dialog
// ---------------------------------------------------------------------------

interface CreateDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCreated: () => void;
}

function CreateKnowledgePageDialog({ open, onOpenChange, onCreated }: CreateDialogProps) {
  const [slug, setSlug] = useState("");
  const [title, setTitle] = useState("");
  const [content, setContent] = useState("");
  const [tags, setTags] = useState("");
  const [submitting, setSubmitting] = useState(false);

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    if (!slug.trim() || !title.trim() || !content.trim()) return;
    setSubmitting(true);
    try {
      await knowledgeApi.create({
        slug: slug.trim(),
        title: title.trim(),
        content: content.trim(),
        tags: tags
          .split(",")
          .map((t) => t.trim())
          .filter(Boolean),
      });
      setSlug("");
      setTitle("");
      setContent("");
      setTags("");
      onOpenChange(false);
      onCreated();
    } catch {
      // Error is visible in console via request() throw
    } finally {
      setSubmitting(false);
    }
  }

  // Auto-generate slug from title
  function handleTitleChange(value: string) {
    setTitle(value);
    if (!slug || slug === slugify(title)) {
      setSlug(slugify(value));
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>New Knowledge Page</DialogTitle>
        </DialogHeader>
        <form onSubmit={(e) => void handleSubmit(e)} className="space-y-4">
          <div className="space-y-1">
            <label className="text-sm font-medium" htmlFor="kp-title">
              Title
            </label>
            <Input
              id="kp-title"
              placeholder="Page title"
              value={title}
              onChange={(e) => handleTitleChange(e.target.value)}
              autoFocus
            />
          </div>
          <div className="space-y-1">
            <label className="text-sm font-medium" htmlFor="kp-slug">
              Slug
            </label>
            <Input
              id="kp-slug"
              placeholder="url-friendly-slug"
              value={slug}
              onChange={(e) => setSlug(e.target.value)}
              className="font-mono text-sm"
            />
          </div>
          <div className="space-y-1">
            <label className="text-sm font-medium" htmlFor="kp-tags">
              Tags <span className="text-muted-foreground font-normal">(comma-separated)</span>
            </label>
            <Input
              id="kp-tags"
              placeholder="rust, architecture, api"
              value={tags}
              onChange={(e) => setTags(e.target.value)}
            />
          </div>
          <div className="space-y-1">
            <label className="text-sm font-medium" htmlFor="kp-content">
              Content <span className="text-muted-foreground font-normal">(markdown)</span>
            </label>
            <textarea
              id="kp-content"
              rows={10}
              placeholder="Write your knowledge page content here…"
              value={content}
              onChange={(e) => setContent(e.target.value)}
              className="flex w-full rounded-md border border-input bg-background px-3 py-2 text-sm font-mono ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 resize-y"
            />
          </div>
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button
              type="submit"
              disabled={!slug.trim() || !title.trim() || !content.trim() || submitting}
            >
              {submitting ? "Creating…" : "Create Page"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function slugify(text: string): string {
  return text
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}
