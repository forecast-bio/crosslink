import { useState } from "react";
import { Card, CardContent } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { formatDateTime } from "@/lib/utils";
import type { Comment, CommentKind } from "@/lib/types";

const COMMENT_KINDS: CommentKind[] = [
  "note", "plan", "decision", "observation", "blocker", "resolution", "result",
];

function kindVariant(kind: CommentKind) {
  switch (kind) {
    case "blocker": return "destructive" as const;
    case "resolution": return "success" as const;
    case "result": return "info" as const;
    case "plan": return "warning" as const;
    default: return "outline" as const;
  }
}

interface CommentThreadProps {
  comments: Comment[];
  onAddComment: (content: string, kind: CommentKind) => Promise<void>;
}

export function CommentThread({ comments, onAddComment }: CommentThreadProps) {
  const [content, setContent] = useState("");
  const [kind, setKind] = useState<CommentKind>("note");
  const [submitting, setSubmitting] = useState(false);

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    if (!content.trim()) return;
    setSubmitting(true);
    try {
      await onAddComment(content.trim(), kind);
      setContent("");
      setKind("note");
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <div className="space-y-3">
      <h2 className="text-sm font-semibold">
        Comments {comments.length > 0 && (
          <span className="text-muted-foreground font-normal ml-1">({comments.length})</span>
        )}
      </h2>

      {comments.length === 0 && (
        <p className="text-xs text-muted-foreground">No comments yet.</p>
      )}

      {comments.map((c) => (
        <Card key={c.id}>
          <CardContent className="pt-4 space-y-1.5">
            <div className="flex items-center gap-2 flex-wrap">
              <Badge variant={kindVariant(c.kind)} className="text-xs capitalize">
                {c.kind}
              </Badge>
              {c.trigger_type && (
                <Badge variant="outline" className="text-xs text-muted-foreground">
                  {c.trigger_type}
                </Badge>
              )}
              <span className="text-xs text-muted-foreground">{formatDateTime(c.created_at)}</span>
            </div>
            <p className="text-sm whitespace-pre-wrap text-muted-foreground">{c.content}</p>
            {c.intervention_context && (
              <p className="text-xs text-muted-foreground/70 italic border-l-2 border-border pl-2">
                Context: {c.intervention_context}
              </p>
            )}
          </CardContent>
        </Card>
      ))}

      <form onSubmit={(e) => void handleSubmit(e)} className="space-y-2 pt-1">
        <textarea
          rows={3}
          placeholder="Add a comment…"
          value={content}
          onChange={(e) => setContent(e.target.value)}
          className="flex w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 resize-none"
        />
        <div className="flex items-center justify-between gap-2">
          <div className="flex gap-1 flex-wrap">
            {COMMENT_KINDS.map((k) => (
              <button
                key={k}
                type="button"
                onClick={() => setKind(k)}
                className={`px-2 py-0.5 rounded text-xs border transition-colors capitalize ${
                  kind === k
                    ? "bg-primary text-primary-foreground border-primary"
                    : "border-border text-muted-foreground hover:border-foreground hover:text-foreground"
                }`}
              >
                {k}
              </button>
            ))}
          </div>
          <Button type="submit" size="sm" disabled={!content.trim() || submitting}>
            {submitting ? "Posting…" : "Comment"}
          </Button>
        </div>
      </form>
    </div>
  );
}
