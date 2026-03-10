/**
 * Lightweight markdown renderer for knowledge page content.
 * Handles: headings, bold, italic, inline code, code blocks, links, lists, blockquotes, horizontal rules.
 * No external dependencies — parses markdown to React elements directly.
 */

import { cn } from "@/lib/utils";

interface MarkdownRendererProps {
  content: string;
  className?: string;
}

interface ParsedLine {
  key: number;
  element: React.ReactNode;
}

function parseInline(text: string): React.ReactNode[] {
  const nodes: React.ReactNode[] = [];
  let remaining = text;
  let key = 0;

  while (remaining.length > 0) {
    // Inline code: `code`
    const codeMatch = remaining.match(/^`([^`]+)`/);
    if (codeMatch) {
      nodes.push(
        <code key={key++} className="rounded bg-muted px-1.5 py-0.5 text-xs font-mono">
          {codeMatch[1]}
        </code>,
      );
      remaining = remaining.slice(codeMatch[0].length);
      continue;
    }

    // Bold+italic: ***text*** or ___text___
    const boldItalicMatch = remaining.match(/^(\*{3}|_{3})(.+?)\1/);
    if (boldItalicMatch) {
      nodes.push(
        <strong key={key++}><em>{parseInline(boldItalicMatch[2])}</em></strong>,
      );
      remaining = remaining.slice(boldItalicMatch[0].length);
      continue;
    }

    // Bold: **text** or __text__
    const boldMatch = remaining.match(/^(\*{2}|_{2})(.+?)\1/);
    if (boldMatch) {
      nodes.push(
        <strong key={key++}>{parseInline(boldMatch[2])}</strong>,
      );
      remaining = remaining.slice(boldMatch[0].length);
      continue;
    }

    // Italic: *text* or _text_
    const italicMatch = remaining.match(/^(\*|_)(.+?)\1/);
    if (italicMatch) {
      nodes.push(
        <em key={key++}>{parseInline(italicMatch[2])}</em>,
      );
      remaining = remaining.slice(italicMatch[0].length);
      continue;
    }

    // Links: [text](url)
    const linkMatch = remaining.match(/^\[([^\]]+)\]\(([^)]+)\)/);
    if (linkMatch) {
      nodes.push(
        <a
          key={key++}
          href={linkMatch[2]}
          target="_blank"
          rel="noopener noreferrer"
          className="text-blue-400 underline hover:text-blue-300"
        >
          {linkMatch[1]}
        </a>,
      );
      remaining = remaining.slice(linkMatch[0].length);
      continue;
    }

    // Plain text: consume until next special character
    const plainMatch = remaining.match(/^[^`*_[\]]+/);
    if (plainMatch) {
      nodes.push(plainMatch[0]);
      remaining = remaining.slice(plainMatch[0].length);
      continue;
    }

    // Consume one character if nothing else matches
    nodes.push(remaining[0]);
    remaining = remaining.slice(1);
  }

  return nodes;
}

export function MarkdownRenderer({ content, className }: MarkdownRendererProps) {
  const lines = content.split("\n");
  const elements: ParsedLine[] = [];
  let key = 0;
  let i = 0;

  while (i < lines.length) {
    const line = lines[i];

    // Fenced code block: ```
    if (line.trimStart().startsWith("```")) {
      const lang = line.trimStart().slice(3).trim();
      const codeLines: string[] = [];
      i++;
      while (i < lines.length && !lines[i].trimStart().startsWith("```")) {
        codeLines.push(lines[i]);
        i++;
      }
      i++; // skip closing ```
      elements.push({
        key: key++,
        element: (
          <div className="rounded-md border border-border bg-muted/50 overflow-x-auto my-2">
            {lang && (
              <div className="border-b border-border px-3 py-1 text-xs text-muted-foreground font-mono">
                {lang}
              </div>
            )}
            <pre className="p-3 text-sm font-mono leading-relaxed">
              <code>{codeLines.join("\n")}</code>
            </pre>
          </div>
        ),
      });
      continue;
    }

    // Heading: # ## ### #### ##### ######
    const headingMatch = line.match(/^(#{1,6})\s+(.+)/);
    if (headingMatch) {
      const level = headingMatch[1].length;
      const text = headingMatch[2];
      const Tag = `h${level}` as keyof JSX.IntrinsicElements;
      const sizes: Record<number, string> = {
        1: "text-xl font-bold mt-6 mb-3",
        2: "text-lg font-bold mt-5 mb-2",
        3: "text-base font-semibold mt-4 mb-2",
        4: "text-sm font-semibold mt-3 mb-1",
        5: "text-sm font-medium mt-2 mb-1",
        6: "text-xs font-medium mt-2 mb-1 text-muted-foreground",
      };
      elements.push({
        key: key++,
        element: <Tag className={sizes[level]}>{parseInline(text)}</Tag>,
      });
      i++;
      continue;
    }

    // Horizontal rule: ---, ***, ___
    if (/^(\s*[-*_]){3,}\s*$/.test(line)) {
      elements.push({
        key: key++,
        element: <hr className="border-border my-4" />,
      });
      i++;
      continue;
    }

    // Blockquote: > text
    if (line.startsWith("> ") || line === ">") {
      const quoteLines: string[] = [];
      while (i < lines.length && (lines[i].startsWith("> ") || lines[i] === ">")) {
        quoteLines.push(lines[i].replace(/^>\s?/, ""));
        i++;
      }
      elements.push({
        key: key++,
        element: (
          <blockquote className="border-l-2 border-muted-foreground/30 pl-3 my-2 text-muted-foreground italic">
            {quoteLines.map((ql, qi) => (
              <p key={qi}>{parseInline(ql)}</p>
            ))}
          </blockquote>
        ),
      });
      continue;
    }

    // Unordered list: - item or * item
    if (/^\s*[-*+]\s/.test(line)) {
      const items: string[] = [];
      while (i < lines.length && /^\s*[-*+]\s/.test(lines[i])) {
        items.push(lines[i].replace(/^\s*[-*+]\s/, ""));
        i++;
      }
      elements.push({
        key: key++,
        element: (
          <ul className="list-disc list-inside space-y-0.5 my-1 text-sm">
            {items.map((item, idx) => (
              <li key={idx}>{parseInline(item)}</li>
            ))}
          </ul>
        ),
      });
      continue;
    }

    // Ordered list: 1. item
    if (/^\s*\d+\.\s/.test(line)) {
      const items: string[] = [];
      while (i < lines.length && /^\s*\d+\.\s/.test(lines[i])) {
        items.push(lines[i].replace(/^\s*\d+\.\s/, ""));
        i++;
      }
      elements.push({
        key: key++,
        element: (
          <ol className="list-decimal list-inside space-y-0.5 my-1 text-sm">
            {items.map((item, idx) => (
              <li key={idx}>{parseInline(item)}</li>
            ))}
          </ol>
        ),
      });
      continue;
    }

    // Empty line → spacer
    if (line.trim() === "") {
      elements.push({ key: key++, element: <div className="h-2" /> });
      i++;
      continue;
    }

    // Paragraph
    elements.push({
      key: key++,
      element: <p className="text-sm leading-relaxed">{parseInline(line)}</p>,
    });
    i++;
  }

  return (
    <div className={cn("space-y-1", className)}>
      {elements.map(({ key: k, element }) => (
        <div key={k}>{element}</div>
      ))}
    </div>
  );
}
