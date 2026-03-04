#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = []
# ///
"""
Crosslink Knowledge MCP Server

An MCP (Model Context Protocol) server that exposes knowledge pages as resources
and provides a search tool. Shells out to the crosslink CLI for all data access.

Usage:
    Registered in .mcp.json as an MCP server.
    Claude reads crosslink://knowledge/<slug> resources and calls search_knowledge tool.
"""

import json
import subprocess
import sys
import io
from typing import Any

# Fix Windows encoding issues
sys.stdin = io.TextIOWrapper(sys.stdin.buffer, encoding='utf-8')
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding='utf-8', line_buffering=True)
sys.stderr = io.TextIOWrapper(sys.stderr.buffer, encoding='utf-8')

SUBPROCESS_TIMEOUT = 10


def log(message: str) -> None:
    """Log to stderr (visible in MCP server logs)."""
    print(f"[knowledge-server] {message}", file=sys.stderr)


def run_crosslink(args: list[str]) -> str | None:
    """Run a crosslink command and return stdout, or None on failure."""
    try:
        result = subprocess.run(
            ["crosslink"] + args,
            capture_output=True,
            text=True,
            timeout=SUBPROCESS_TIMEOUT,
        )
        if result.returncode == 0:
            return result.stdout.strip()
        log(f"crosslink {' '.join(args)} failed: {result.stderr.strip()}")
        return None
    except subprocess.TimeoutExpired:
        log(f"crosslink {' '.join(args)} timed out")
        return None
    except FileNotFoundError:
        log("crosslink binary not found")
        return None
    except Exception as e:
        log(f"crosslink error: {e}")
        return None


def list_knowledge_pages() -> list[dict]:
    """Get knowledge pages as JSON list via crosslink CLI."""
    output = run_crosslink(["knowledge", "list", "--json"])
    if not output:
        return []
    try:
        return json.loads(output)
    except json.JSONDecodeError as e:
        log(f"Failed to parse knowledge list JSON: {e}")
        return []


def get_page_content(slug: str) -> str | None:
    """Get the full content of a knowledge page."""
    return run_crosslink(["knowledge", "show", slug])


def search_knowledge(query: str, tag: str | None = None, since: str | None = None) -> str | None:
    """Search knowledge pages and return JSON results."""
    args = ["knowledge", "search", query, "--json"]
    if tag:
        args.extend(["--tag", tag])
    if since:
        args.extend(["--since", since])
    return run_crosslink(args)


# MCP Protocol Implementation

TOOL_DEFINITION = {
    'name': 'search_knowledge',
    'description': (
        'Search crosslink knowledge pages by content. '
        'Returns matching snippets with context lines. '
        'Optionally filter by tag or date.'
    ),
    'inputSchema': {
        'type': 'object',
        'properties': {
            'query': {
                'type': 'string',
                'description': 'Search query (case-insensitive substring match)',
            },
            'tag': {
                'type': 'string',
                'description': 'Optional: filter results to pages with this tag',
            },
            'since': {
                'type': 'string',
                'description': 'Optional: filter to pages updated since this date (YYYY-MM-DD)',
            },
        },
        'required': ['query'],
    },
}


def handle_request(request: dict[str, Any]) -> dict[str, Any] | None:
    """Handle an MCP JSON-RPC request."""
    method = request.get('method', '')
    request_id = request.get('id')
    params = request.get('params', {})

    if method == 'initialize':
        return {
            'jsonrpc': '2.0',
            'id': request_id,
            'result': {
                'protocolVersion': '2024-11-05',
                'capabilities': {
                    'resources': {},
                    'tools': {},
                },
                'serverInfo': {
                    'name': 'crosslink-knowledge',
                    'version': '1.0.0',
                },
            },
        }

    elif method == 'notifications/initialized':
        return None

    elif method == 'resources/list':
        pages = list_knowledge_pages()
        resources = []
        for page in pages:
            slug = page.get('slug', '')
            title = page.get('title', slug)
            tags = page.get('tags', [])
            desc = f"Knowledge page: {title}"
            if tags:
                desc += f" (tags: {', '.join(tags)})"
            resources.append({
                'uri': f'crosslink://knowledge/{slug}',
                'name': title,
                'description': desc,
                'mimeType': 'text/markdown',
            })
        return {
            'jsonrpc': '2.0',
            'id': request_id,
            'result': {'resources': resources},
        }

    elif method == 'resources/read':
        uri = params.get('uri', '')
        prefix = 'crosslink://knowledge/'
        if not uri.startswith(prefix):
            return {
                'jsonrpc': '2.0',
                'id': request_id,
                'error': {
                    'code': -32602,
                    'message': f'Invalid resource URI: {uri}',
                },
            }
        slug = uri[len(prefix):]
        content = get_page_content(slug)
        if content is None:
            return {
                'jsonrpc': '2.0',
                'id': request_id,
                'error': {
                    'code': -32602,
                    'message': f'Knowledge page not found: {slug}',
                },
            }
        return {
            'jsonrpc': '2.0',
            'id': request_id,
            'result': {
                'contents': [{
                    'uri': uri,
                    'mimeType': 'text/markdown',
                    'text': content,
                }],
            },
        }

    elif method == 'tools/list':
        return {
            'jsonrpc': '2.0',
            'id': request_id,
            'result': {'tools': [TOOL_DEFINITION]},
        }

    elif method == 'tools/call':
        tool_name = params.get('name', '')
        arguments = params.get('arguments', {})

        if tool_name == 'search_knowledge':
            query = arguments.get('query', '')
            if not query:
                return {
                    'jsonrpc': '2.0',
                    'id': request_id,
                    'result': {
                        'content': [{'type': 'text', 'text': 'Error: query is required'}],
                        'isError': True,
                    },
                }
            result = search_knowledge(
                query,
                tag=arguments.get('tag'),
                since=arguments.get('since'),
            )
            if result is None:
                return {
                    'jsonrpc': '2.0',
                    'id': request_id,
                    'result': {
                        'content': [{'type': 'text', 'text': 'No results or search failed'}],
                    },
                }
            return {
                'jsonrpc': '2.0',
                'id': request_id,
                'result': {
                    'content': [{'type': 'text', 'text': result}],
                },
            }
        else:
            return {
                'jsonrpc': '2.0',
                'id': request_id,
                'error': {
                    'code': -32601,
                    'message': f'Unknown tool: {tool_name}',
                },
            }

    else:
        return {
            'jsonrpc': '2.0',
            'id': request_id,
            'error': {
                'code': -32601,
                'message': f'Method not found: {method}',
            },
        }


def main():
    """Main MCP server loop - reads JSON-RPC from stdin, writes to stdout."""
    log("Starting knowledge MCP server")

    while True:
        try:
            line = sys.stdin.readline()
            if not line:
                break

            line = line.strip()
            if not line:
                continue

            request = json.loads(line)
            response = handle_request(request)

            if response is not None:
                print(json.dumps(response), flush=True)

        except json.JSONDecodeError as e:
            log(f"JSON decode error: {e}")
            error_response = {
                'jsonrpc': '2.0',
                'id': None,
                'error': {
                    'code': -32700,
                    'message': 'Parse error',
                },
            }
            print(json.dumps(error_response), flush=True)
        except Exception as e:
            log(f"Unexpected error: {e}")
            break

    log("Server shutting down")


if __name__ == '__main__':
    main()
