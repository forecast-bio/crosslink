import { useEffect, useState } from "react";
import { useParams, Link } from "react-router";
import { ArrowLeft, CircleDot, CheckCircle2, X, Plus } from "lucide-react";
import { useIssuesStore } from "@/stores/issues";
import { issues as issuesApi } from "@/api/client";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { CommentThread } from "@/components/CommentThread";
import { formatRelativeTime, priorityVariant } from "@/lib/utils";
import type { IssueDetail as IssueDetailType, CommentKind } from "@/lib/types";

export function IssueDetail() {
  const { id } = useParams<{ id: string }>();
  const numId = Number(id);
  const { detail, fetchDetail } = useIssuesStore();
  const issue: IssueDetailType | undefined = detail[numId];
  const [newLabel, setNewLabel] = useState("");
  const [addingLabel, setAddingLabel] = useState(false);

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

  const handleAddLabel = async (e: React.FormEvent) => {
    e.preventDefault();
    const label = newLabel.trim();
    if (!label) return;
    await issuesApi.addLabel(numId, label);
    setNewLabel("");
    setAddingLabel(false);
    void fetchDetail(numId);
  };

  const handleRemoveLabel = async (label: string) => {
    await issuesApi.removeLabel(numId, label);
    void fetchDetail(numId);
  };

  const handleAddComment = async (content: string, kind: CommentKind) => {
    await issuesApi.addComment(numId, { content, kind });
    void fetchDetail(numId);
  };

  return (
    <div className="p-6 space-y-5 max-w-3xl">
      {/* Breadcrumb / back */}
      <div className="flex items-center gap-2">
        <Link to="/issues">
          <Button variant="ghost" size="icon" className="h-8 w-8">
            <ArrowLeft className="h-4 w-4" />
          </Button>
        </Link>
        <span className="text-muted-foreground font-mono text-sm">#{issue.id}</span>
        {issue.status === "open" ? (
          <CircleDot className="h-4 w-4 text-green-400" />
        ) : (
          <CheckCircle2 className="h-4 w-4 text-muted-foreground" />
        )}
        <span className="text-xs text-muted-foreground">
          Updated {formatRelativeTime(issue.updated_at)}
        </span>
      </div>

      {/* Title + meta */}
      <div className="space-y-3">
        <h1 className="text-xl font-bold leading-tight">{issue.title}</h1>

        <div className="flex items-center gap-2 flex-wrap">
          <Badge variant={issue.status === "open" ? "success" : "secondary"} className="capitalize">
            {issue.status}
          </Badge>
          <Badge variant={priorityVariant(issue.priority)} className="capitalize">
            {issue.priority}
          </Badge>

          {/* Label chips */}
          {issue.labels.map((l) => (
            <span
              key={l}
              className="inline-flex items-center gap-1 px-2 py-0.5 rounded-full border border-border text-xs bg-secondary text-secondary-foreground"
            >
              {l}
              <button
                onClick={() => void handleRemoveLabel(l)}
                className="hover:text-destructive transition-colors"
                aria-label={`Remove label ${l}`}
              >
                <X className="h-3 w-3" />
              </button>
            </span>
          ))}

          {/* Add label */}
          {addingLabel ? (
            <form onSubmit={(e) => void handleAddLabel(e)} className="flex items-center gap-1">
              <Input
                autoFocus
                value={newLabel}
                onChange={(e) => setNewLabel(e.target.value)}
                placeholder="label"
                className="h-6 px-2 text-xs w-24"
                onBlur={() => { if (!newLabel) setAddingLabel(false); }}
              />
              <Button type="submit" size="sm" className="h-6 px-2 text-xs">Add</Button>
              <Button
                type="button"
                variant="ghost"
                size="sm"
                className="h-6 px-1"
                onClick={() => { setAddingLabel(false); setNewLabel(""); }}
              >
                <X className="h-3 w-3" />
              </Button>
            </form>
          ) : (
            <button
              onClick={() => setAddingLabel(true)}
              className="inline-flex items-center gap-0.5 px-2 py-0.5 rounded-full border border-dashed border-border text-xs text-muted-foreground hover:border-foreground hover:text-foreground transition-colors"
            >
              <Plus className="h-3 w-3" /> label
            </button>
          )}

          <Button size="sm" variant="outline" className="ml-auto" onClick={() => void handleToggle()}>
            {issue.status === "open" ? "Close issue" : "Reopen issue"}
          </Button>
        </div>
      </div>

      {/* Description */}
      {issue.description && (
        <Card>
          <CardContent className="pt-4 text-sm whitespace-pre-wrap text-muted-foreground">
            {issue.description}
          </CardContent>
        </Card>
      )}

      {/* Dependencies */}
      {(issue.blockers.length > 0 || issue.blocking.length > 0) && (
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm">Dependencies</CardTitle>
          </CardHeader>
          <CardContent className="space-y-2 text-sm pt-0">
            {issue.blockers.length > 0 && (
              <div className="flex items-start gap-2">
                <span className="text-muted-foreground shrink-0">Blocked by:</span>
                <div className="flex flex-wrap gap-1">
                  {issue.blockers.map((bid) => (
                    <Link
                      key={bid}
                      to={`/issues/${bid}`}
                      className="inline-flex items-center gap-1 px-2 py-0.5 rounded border border-border text-xs hover:bg-accent transition-colors font-mono"
                    >
                      <CircleDot className="h-3 w-3 text-red-400" />
                      #{bid}
                    </Link>
                  ))}
                </div>
              </div>
            )}
            {issue.blocking.length > 0 && (
              <div className="flex items-start gap-2">
                <span className="text-muted-foreground shrink-0">Blocking:</span>
                <div className="flex flex-wrap gap-1">
                  {issue.blocking.map((bid) => (
                    <Link
                      key={bid}
                      to={`/issues/${bid}`}
                      className="inline-flex items-center gap-1 px-2 py-0.5 rounded border border-border text-xs hover:bg-accent transition-colors font-mono"
                    >
                      #{bid}
                    </Link>
                  ))}
                </div>
              </div>
            )}
          </CardContent>
        </Card>
      )}

      {/* Subissues */}
      {issue.subissues.length > 0 && (
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm">
              Subissues
              <span className="text-muted-foreground font-normal ml-1">
                ({issue.subissues.filter((s) => s.status === "closed").length}/{issue.subissues.length} closed)
              </span>
            </CardTitle>
          </CardHeader>
          <CardContent className="space-y-1 pt-0">
            {issue.subissues.map((sub) => (
              <Link
                key={sub.id}
                to={`/issues/${sub.id}`}
                className="flex items-center gap-2 text-sm rounded px-2 py-1 hover:bg-accent transition-colors"
              >
                {sub.status === "open" ? (
                  <CircleDot className="h-3 w-3 text-green-400 shrink-0" />
                ) : (
                  <CheckCircle2 className="h-3 w-3 text-muted-foreground shrink-0" />
                )}
                <span className="font-mono text-xs text-muted-foreground">#{sub.id}</span>
                <span className={sub.status === "closed" ? "line-through text-muted-foreground" : ""}>
                  {sub.title}
                </span>
              </Link>
            ))}
          </CardContent>
        </Card>
      )}

      {/* Comment thread */}
      <CommentThread comments={issue.comments} onAddComment={handleAddComment} />
    </div>
  );
}
