import { useEffect, useState } from "react";
import { Link } from "react-router";
import { knowledge as knowledgeApi } from "@/api/client";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import { formatRelativeTime } from "@/lib/utils";
import type { KnowledgePage } from "@/lib/types";

export function Knowledge() {
  const [pages, setPages] = useState<KnowledgePage[]>([]);
  const [loading, setLoading] = useState(true);
  const [search, setSearch] = useState("");

  useEffect(() => {
    knowledgeApi
      .list()
      .then(setPages)
      .catch(() => {})
      .finally(() => setLoading(false));
  }, []);

  const filtered = pages.filter(
    (p) =>
      search === "" ||
      p.title.toLowerCase().includes(search.toLowerCase()) ||
      p.slug.includes(search.toLowerCase()) ||
      p.tags.some((t) => t.includes(search.toLowerCase())),
  );

  return (
    <div className="p-6 space-y-4">
      <h1 className="text-2xl font-bold">Knowledge</h1>
      <Input
        placeholder="Search pages…"
        value={search}
        onChange={(e) => setSearch(e.target.value)}
        className="max-w-xs"
      />
      {loading ? (
        <p className="text-muted-foreground text-sm">Loading…</p>
      ) : filtered.length === 0 ? (
        <p className="text-muted-foreground text-sm">No pages found.</p>
      ) : (
        <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
          {filtered.map((page) => (
            <Link key={page.slug} to={`/knowledge/${encodeURIComponent(page.slug)}`}>
              <Card className="hover:bg-accent/30 transition-colors cursor-pointer h-full">
                <CardHeader className="pb-2">
                  <CardTitle className="text-sm">{page.title}</CardTitle>
                  <p className="text-xs text-muted-foreground font-mono">{page.slug}</p>
                </CardHeader>
                <CardContent className="space-y-2">
                  <div className="flex flex-wrap gap-1">
                    {page.tags.map((t) => (
                      <Badge key={t} variant="secondary" className="text-xs">{t}</Badge>
                    ))}
                  </div>
                  <p className="text-xs text-muted-foreground">
                    Updated {formatRelativeTime(page.updated)}
                  </p>
                </CardContent>
              </Card>
            </Link>
          ))}
        </div>
      )}
    </div>
  );
}
