import { useEffect, useState } from "react";
import { useParams, Link } from "react-router";
import { ArrowLeft } from "lucide-react";
import { knowledge as knowledgeApi } from "@/api/client";
import { Card, CardContent } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { formatDateTime } from "@/lib/utils";
import type { KnowledgePage } from "@/lib/types";

export function KnowledgeDetail() {
  const { slug } = useParams<{ slug: string }>();
  const [page, setPage] = useState<KnowledgePage | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    if (!slug) return;
    knowledgeApi
      .get(decodeURIComponent(slug))
      .then(setPage)
      .catch(() => {})
      .finally(() => setLoading(false));
  }, [slug]);

  if (loading) return <div className="p-6 text-muted-foreground">Loading…</div>;
  if (!page) return (
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
      <div className="flex items-center gap-3">
        <Link to="/knowledge">
          <Button variant="ghost" size="icon">
            <ArrowLeft className="h-4 w-4" />
          </Button>
        </Link>
        <div>
          <h1 className="text-xl font-bold">{page.title}</h1>
          <p className="font-mono text-xs text-muted-foreground">{page.slug}</p>
        </div>
      </div>

      <div className="flex items-center gap-2 flex-wrap text-xs text-muted-foreground">
        {page.tags.map((t) => (
          <Badge key={t} variant="secondary">{t}</Badge>
        ))}
        {page.sources.length > 0 && (
          <span>Source: <span className="font-mono">{page.sources[0].url}</span></span>
        )}
        <span>Updated {formatDateTime(page.updated)}</span>
      </div>

      <Card>
        <CardContent className="pt-4">
          <pre className="whitespace-pre-wrap text-sm text-muted-foreground font-mono leading-relaxed">
            {page.content}
          </pre>
        </CardContent>
      </Card>
    </div>
  );
}
