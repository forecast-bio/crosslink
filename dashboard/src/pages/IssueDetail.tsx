import { useEffect, useState } from "react";
import { useParams, Link } from "react-router";
import { ArrowLeft, CircleDot, CheckCircle2 } from "lucide-react";
import { useIssuesStore } from "@/stores/issues";
import { issues as issuesApi } from "@/api/client";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { formatDateTime, formatRelativeTime } from "@/lib/utils";
import type { IssueDetail as IssueDetailType } from "@/lib/types";

export function IssueDetail() {
  const { id } = useParams<{ id: string }>();
  const numId = Number(id);
  const { detail, fetchDetail } = useIssuesStore();
  const issue: IssueDetailType | undefined = detail[numId];
  const [newComment, setNewComment] = useState("");
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    if (numId) void fetchDetail(numId);
  }, [numId, fetchDetail]);

  if (!issue) {
    return <div className="p-6 text-muted-foreground">Loading…</div>;
  }

  const handleToggle = async () => {
    if (issue.status === "open") {
      await issuesApi.close(numId);
    } else {
      await issuesApi.reopen(numId);
    }
    void fetchDetail(numId);
  };

  const handleComment = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!newComment.trim()) return;
    setSubmitting(true);
    await issuesApi.addComment(numId, { content: newComment });
    setNewComment("");
    setSubmitting(false);
    void fetchDetail(numId);
  };

  return (
    <div className="p-6 space-y-4 max-w-3xl">
      <div className="flex items-center gap-3">
        <Link to="/issues">
          <Button variant="ghost" size="icon">
            <ArrowLeft className="h-4 w-4" />
          </Button>
        </Link>
        <span className="text-muted-foreground font-mono text-sm">#{issue.id}</span>
        {issue.status === "open" ? (
          <CircleDot className="h-4 w-4 text-green-400" />
        ) : (
          <CheckCircle2 className="h-4 w-4 text-muted-foreground" />
        )}
      </div>

      <div>
        <h1 className="text-xl font-bold">{issue.title}</h1>
        <div className="flex items-center gap-2 mt-2 flex-wrap">
          <Badge variant={issue.status === "open" ? "success" : "secondary"}>
            {issue.status}
          </Badge>
          <Badge variant="outline">{issue.priority}</Badge>
          {issue.labels.map((l) => (
            <Badge key={l} variant="secondary">{l}</Badge>
          ))}
          <span className="text-xs text-muted-foreground">
            Updated {formatRelativeTime(issue.updated_at)}
          </span>
          <Button size="sm" variant="outline" onClick={handleToggle}>
            {issue.status === "open" ? "Close" : "Reopen"}
          </Button>
        </div>
      </div>

      {issue.description && (
        <Card>
          <CardContent className="pt-4 text-sm whitespace-pre-wrap text-muted-foreground">
            {issue.description}
          </CardContent>
        </Card>
      )}

      {(issue.blockers.length > 0 || issue.blocking.length > 0) && (
        <Card>
          <CardHeader><CardTitle className="text-sm">Dependencies</CardTitle></CardHeader>
          <CardContent className="space-y-2 text-sm">
            {issue.blockers.length > 0 && (
              <div>
                <span className="text-muted-foreground">Blocked by: </span>
                {issue.blockers.map((bid) => (
                  <Link key={bid} to={`/issues/${bid}`} className="text-blue-400 hover:underline mr-2">
                    #{bid}
                  </Link>
                ))}
              </div>
            )}
            {issue.blocking.length > 0 && (
              <div>
                <span className="text-muted-foreground">Blocking: </span>
                {issue.blocking.map((bid) => (
                  <Link key={bid} to={`/issues/${bid}`} className="text-blue-400 hover:underline mr-2">
                    #{bid}
                  </Link>
                ))}
              </div>
            )}
          </CardContent>
        </Card>
      )}

      {issue.subissues.length > 0 && (
        <Card>
          <CardHeader><CardTitle className="text-sm">Subissues</CardTitle></CardHeader>
          <CardContent className="space-y-1">
            {issue.subissues.map((sub) => (
              <Link key={sub.id} to={`/issues/${sub.id}`} className="flex items-center gap-2 text-sm hover:text-blue-400">
                {sub.status === "open" ? (
                  <CircleDot className="h-3 w-3 text-green-400" />
                ) : (
                  <CheckCircle2 className="h-3 w-3 text-muted-foreground" />
                )}
                <span className="font-mono text-xs text-muted-foreground">#{sub.id}</span>
                {sub.title}
              </Link>
            ))}
          </CardContent>
        </Card>
      )}

      <div className="space-y-3">
        <h2 className="text-sm font-semibold">Comments</h2>
        {issue.comments.length === 0 && (
          <p className="text-xs text-muted-foreground">No comments yet.</p>
        )}
        {issue.comments.map((c) => (
          <Card key={c.id}>
            <CardContent className="pt-4 text-sm space-y-1">
              <div className="flex items-center gap-2">
                {c.kind && <Badge variant="outline" className="text-xs">{c.kind}</Badge>}
                <span className="text-xs text-muted-foreground">{formatDateTime(c.created_at)}</span>
              </div>
              <p className="whitespace-pre-wrap text-muted-foreground">{c.content}</p>
            </CardContent>
          </Card>
        ))}
        <form onSubmit={handleComment} className="flex gap-2">
          <Input
            placeholder="Add a comment…"
            value={newComment}
            onChange={(e) => setNewComment(e.target.value)}
            className="flex-1"
          />
          <Button type="submit" size="sm" disabled={submitting || !newComment.trim()}>
            Comment
          </Button>
        </form>
      </div>
    </div>
  );
}
