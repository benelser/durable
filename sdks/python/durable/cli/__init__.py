"""Durable CLI — operational tools for agent execution.

The sqlite3 shell equivalent for durable agent execution.

Usage:
    durable status                    Show all executions
    durable inspect <id>              Detailed execution view
    durable steps <id>                Step-by-step timeline
    durable cost [id]                 Cost breakdown
    durable compact [--all]           Compact event logs
    durable replay <id>              Step-by-step replay with timing
    durable export <id> [-o file]     Export execution as JSON
    durable health                    Storage health check
"""
