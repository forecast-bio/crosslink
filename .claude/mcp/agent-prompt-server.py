#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = []
# ///
"""
Crosslink Agent Prompt MCP Server

An MCP server that provides reliable prompt delivery to tmux-based agent sessions.
Wraps `crosslink agent prompt` to avoid the pitfalls of raw `tmux send-keys`.

Usage:
    Registered in .claude/settings.json as an MCP server.
    Claude calls mcp__crosslink-agent-prompt__agent_prompt(session, prompt) to send prompts.
"""

import json
import sys
import io
import subprocess
from typing import Any

# Fix Windows encoding issues
sys.stdin = io.TextIOWrapper(sys.stdin.buffer, encoding='utf-8')
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding='utf-8', line_buffering=True)
sys.stderr = io.TextIOWrapper(sys.stderr.buffer, encoding='utf-8')


def log(message: str) -> None:
    """Log to stderr (visible in MCP server logs)."""
    print(f"[agent-prompt] {message}", file=sys.stderr)


TOOL_DEFINITION = {
    'name': 'agent_prompt',
    'description': (
        'Send a prompt to a running tmux-based agent session. '
        'Uses tmux load-buffer + paste-buffer for reliable delivery — '
        'no newline mangling, no length limits, no shell escaping issues. '
        'Prefer this over raw tmux send-keys for agent-to-agent communication.'
    ),
    'inputSchema': {
        'type': 'object',
        'properties': {
            'session': {
                'type': 'string',
                'description': 'Agent slug or tmux session name'
            },
            'prompt': {
                'type': 'string',
                'description': 'The full prompt text to send (supports multiline, any length)'
            },
            'submit': {
                'type': 'boolean',
                'description': 'Whether to press Enter after pasting to submit the prompt (default: true)',
                'default': True
            }
        },
        'required': ['session', 'prompt']
    }
}


def handle_agent_prompt(arguments: dict[str, Any]) -> dict[str, Any]:
    """Handle the agent_prompt tool call by delegating to crosslink CLI."""
    session = arguments.get('session', '').strip()
    prompt = arguments.get('prompt', '')
    submit = arguments.get('submit', True)

    if not session:
        return {
            'content': [{'type': 'text', 'text': 'Error: session is required'}],
            'isError': True
        }

    if not prompt:
        return {
            'content': [{'type': 'text', 'text': 'Error: prompt is required'}],
            'isError': True
        }

    cmd = ['crosslink', 'agent', 'prompt', session, prompt]
    if not submit:
        cmd.append('--no-submit')

    try:
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=10
        )

        if result.returncode == 0:
            output = result.stdout.strip() or f"Prompt sent to session '{session}'"
            return {
                'content': [{'type': 'text', 'text': output}]
            }
        else:
            error = result.stderr.strip() or result.stdout.strip() or 'Unknown error'
            return {
                'content': [{'type': 'text', 'text': f'Error: {error}'}],
                'isError': True
            }

    except subprocess.TimeoutExpired:
        return {
            'content': [{'type': 'text', 'text': 'Error: command timed out after 10 seconds'}],
            'isError': True
        }
    except FileNotFoundError:
        return {
            'content': [{'type': 'text', 'text': 'Error: crosslink binary not found'}],
            'isError': True
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
                    'tools': {}
                },
                'serverInfo': {
                    'name': 'crosslink-agent-prompt',
                    'version': '1.0.0'
                }
            }
        }

    elif method == 'notifications/initialized':
        return None

    elif method == 'tools/list':
        return {
            'jsonrpc': '2.0',
            'id': request_id,
            'result': {
                'tools': [TOOL_DEFINITION]
            }
        }

    elif method == 'tools/call':
        tool_name = params.get('name', '')
        arguments = params.get('arguments', {})

        if tool_name == 'agent_prompt':
            result = handle_agent_prompt(arguments)
            return {
                'jsonrpc': '2.0',
                'id': request_id,
                'result': result
            }
        else:
            return {
                'jsonrpc': '2.0',
                'id': request_id,
                'error': {
                    'code': -32601,
                    'message': f'Unknown tool: {tool_name}'
                }
            }

    else:
        return {
            'jsonrpc': '2.0',
            'id': request_id,
            'error': {
                'code': -32601,
                'message': f'Method not found: {method}'
            }
        }


def main():
    """Main MCP server loop - reads JSON-RPC from stdin, writes to stdout."""
    log("Starting agent-prompt MCP server")

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
                    'message': 'Parse error'
                }
            }
            print(json.dumps(error_response), flush=True)
        except Exception as e:
            log(f"Unexpected error: {e}")


if __name__ == '__main__':
    main()
