#!/usr/bin/env python3
"""
07 — Embedded Runtime: Durable agents inside a FastAPI app.

This is the full picture. A production-shaped API server with:
  - One Runtime, multiple agents running as durable threads
  - Non-blocking spawn: POST /orders returns immediately
  - Signal-based approval: POST /orders/{id}/approve
  - Auto-resume: the event loop handles resumption, not your code
  - Lifecycle callbacks: Slack-style notifications on suspend/complete

Run it:
    pip install fastapi uvicorn
    export OPENAI_API_KEY=sk-...
    python 07_fastapi_embedded.py

Then in another terminal:
    # Create an order (returns immediately)
    curl -X POST http://localhost:8000/orders \\
      -H 'Content-Type: application/json' \\
      -d '{"customer": "alice", "item": "WIDGET-X", "quantity": 3}'

    # Check status
    curl http://localhost:8000/orders/{execution_id}

    # Approve the payment (agent auto-resumes)
    curl -X POST http://localhost:8000/orders/{execution_id}/approve

Or without FastAPI (runs the demo inline):
    python 07_fastapi_embedded.py --mock
"""

import os
import sys
import time
import shutil

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from durable import Runtime, Agent, tool, Budget

DATA_DIR = "./demo-data/fastapi"

# ---------------------------------------------------------------------------
# Tools — these are real side effects
# ---------------------------------------------------------------------------

SIDE_EFFECTS = {"inventory": 0, "charges": 0, "emails": 0}


@tool("check_inventory", description="Check if items are in stock and get pricing.")
def check_inventory(item_id: str, quantity: int) -> dict:
    SIDE_EFFECTS["inventory"] += 1
    print(f"    [check_inventory #{SIDE_EFFECTS['inventory']}] {quantity}x {item_id}")
    return {"item_id": item_id, "in_stock": True, "available": 500, "unit_price": 49.99}


@tool(
    "charge_payment",
    description="Charge customer payment. This is a real financial transaction. Requires human approval.",
    requires_confirmation=True,
)
def charge_payment(customer_id: str, amount: float) -> dict:
    SIDE_EFFECTS["charges"] += 1
    print(f"    [charge_payment #{SIDE_EFFECTS['charges']}] ${amount:.2f} to {customer_id}")
    return {"transaction_id": f"txn_{SIDE_EFFECTS['charges']}", "amount": amount, "status": "charged"}


@tool("send_confirmation_email", description="Send order confirmation email to customer.")
def send_confirmation_email(to: str, order_summary: str) -> dict:
    SIDE_EFFECTS["emails"] += 1
    print(f"    [send_email #{SIDE_EFFECTS['emails']}] to={to}")
    return {"sent": True, "message_id": f"msg_{SIDE_EFFECTS['emails']}"}


# ---------------------------------------------------------------------------
# Mock LLM (for --mock mode)
# ---------------------------------------------------------------------------

MOCK_STEP = 0


def mock_llm(messages, tools=None, model=None):
    global MOCK_STEP
    MOCK_STEP += 1
    if MOCK_STEP == 1:
        return {"tool_calls": [{"id": "c1", "name": "check_inventory",
                "arguments": {"item_id": "WIDGET-X", "quantity": 3}}]}
    elif MOCK_STEP == 2:
        return {"tool_calls": [{"id": "c2", "name": "charge_payment",
                "arguments": {"customer_id": "alice", "amount": 149.97}}]}
    elif MOCK_STEP == 3:
        return {"tool_calls": [{"id": "c3", "name": "send_confirmation_email",
                "arguments": {"to": "alice@example.com",
                              "order_summary": "3x WIDGET-X, $149.97"}}]}
    return {"content": "Order processed: 3x WIDGET-X, $149.97 charged to alice, confirmation sent."}


# ---------------------------------------------------------------------------
# Application: embedded runtime
# ---------------------------------------------------------------------------

def run_mock_demo():
    """Run the demo without FastAPI, simulating the API flow."""
    shutil.rmtree(DATA_DIR, ignore_errors=True)

    print("=" * 64)
    print("  Embedded Runtime Demo (Mock Mode)")
    print("  Simulates: FastAPI + Durable Runtime + Durable Threads")
    print("=" * 64)

    # --- Boot: one runtime, embedded in the app ---
    rt = Runtime(DATA_DIR)

    order_agent = Agent(
        DATA_DIR,
        runtime=rt,
        agent_id="order-processor",
        system_prompt=(
            "You are an order processing agent. Process orders by:\n"
            "1. Check inventory with check_inventory\n"
            "2. Charge payment with charge_payment\n"
            "3. Send confirmation email with send_confirmation_email\n"
            "Call each tool once in order, then summarize."
        ),
    )
    order_agent.add_tool(check_inventory)
    order_agent.add_tool(charge_payment)
    order_agent.add_tool(send_confirmation_email)
    order_agent.budget = Budget(max_dollars=5.00, max_llm_calls=20)

    if "--mock" not in sys.argv:
        from durable.providers import OpenAI
        order_agent.set_llm(OpenAI())
    else:
        order_agent.set_llm(mock_llm)

    # --- Lifecycle callbacks (in production: Slack, PagerDuty, etc.) ---
    suspended_reasons = {}

    @rt.on_suspend
    def handle_suspend(agent_id, exec_id, reason):
        reason_type = reason.get("type", "unknown")
        print(f"\n  ** SUSPENDED: agent={agent_id} exec={exec_id[:12]}... reason={reason_type}")
        if reason_type == "waiting_for_confirmation":
            conf_id = reason.get("confirmation_id", "")
            suspended_reasons[exec_id] = conf_id
            print(f"     Confirmation needed: {conf_id}")
            print(f"     In production: send Slack message, email, or dashboard notification")

    @rt.on_complete
    def handle_complete(agent_id, exec_id, response):
        print(f"\n  ** COMPLETED: agent={agent_id} exec={exec_id[:12]}...")
        print(f"     Response: {response[:80]}")

    # --- Simulate: POST /orders ---
    print("\n--- POST /orders (non-blocking spawn) ---\n")
    exec_id = rt.go(order_agent, "Process order: 3x WIDGET-X for customer alice, email alice@example.com")
    print(f"  Returned immediately. execution_id: {exec_id}")
    print(f"  Agent running in background as a durable thread.")

    # --- Wait for agent to suspend for payment approval ---
    print("\n--- Waiting for agent to reach payment confirmation gate ---\n")
    time.sleep(3)

    print(f"  Side effects so far:")
    print(f"    Inventory checks: {SIDE_EFFECTS['inventory']}")
    print(f"    Payments charged: {SIDE_EFFECTS['charges']} (should be 0 — waiting for approval)")
    print(f"    Emails sent:      {SIDE_EFFECTS['emails']}")

    # --- Simulate: POST /orders/{id}/approve ---
    if exec_id in suspended_reasons:
        conf_id = suspended_reasons[exec_id]
        print(f"\n--- POST /orders/{exec_id[:12]}.../approve ---\n")
        print(f"  Sending approval signal: {conf_id}")
        rt.signal(exec_id, conf_id, True)
        print(f"  Signal sent. Event loop will auto-resume. No resume() call.")
    else:
        # Fallback: construct deterministically
        conf_id = f"confirm_charge_payment_{exec_id}_3"
        print(f"\n--- Sending approval (constructed ID) ---\n")
        rt.signal(exec_id, conf_id, True)

    # --- Wait for completion ---
    print("\n--- Waiting for auto-resume and completion ---\n")
    deadline = time.time() + 15
    while SIDE_EFFECTS["charges"] == 0 and time.time() < deadline:
        time.sleep(0.5)
    time.sleep(2)  # let email send and completion propagate

    # --- Results ---
    print(f"\n{'=' * 64}")
    print("  RESULTS")
    print(f"{'=' * 64}")
    print(f"  Inventory checks: {SIDE_EFFECTS['inventory']}")
    print(f"  Payments charged: {SIDE_EFFECTS['charges']}")
    print(f"  Emails sent:      {SIDE_EFFECTS['emails']}")
    print()

    if SIDE_EFFECTS["charges"] == 1 and SIDE_EFFECTS["emails"] == 1:
        print("  All side effects executed exactly once.")
        print("  Payment only charged AFTER human approval.")
        print("  No resume() call — event loop handled it.")
    else:
        print("  Something didn't complete. Check output above.")

    print(f"\n  This is one Runtime, one agent type, running as a durable thread")
    print(f"  inside your application process. Scale to 100 agents by spawning")
    print(f"  more rt.go() calls. Suspended agents cost zero threads.")

    rt.shutdown()
    shutil.rmtree(DATA_DIR, ignore_errors=True)


def run_fastapi_server():
    """Run the actual FastAPI server."""
    try:
        from fastapi import FastAPI, HTTPException
        from pydantic import BaseModel
        import uvicorn
    except ImportError:
        print("FastAPI not installed. Run: pip install fastapi uvicorn")
        print("Or use --mock mode: python 07_fastapi_embedded.py --mock")
        sys.exit(1)

    app = FastAPI(title="Durable Order Processing API")
    rt = Runtime(DATA_DIR)

    # Agent template
    order_agent = Agent(
        DATA_DIR,
        runtime=rt,
        agent_id="order-processor",
        system_prompt=(
            "You are an order processing agent. Process orders by:\n"
            "1. Check inventory\n2. Charge payment\n3. Send confirmation email"
        ),
    )
    order_agent.add_tool(check_inventory)
    order_agent.add_tool(charge_payment)
    order_agent.add_tool(send_confirmation_email)

    from durable.providers import OpenAI
    order_agent.set_llm(OpenAI())

    # Track suspended executions
    pending_approvals = {}

    @rt.on_suspend
    def on_suspend(agent_id, exec_id, reason):
        if reason.get("type") == "waiting_for_confirmation":
            pending_approvals[exec_id] = reason.get("confirmation_id", "")

    class OrderRequest(BaseModel):
        customer: str
        item: str
        quantity: int

    @app.post("/orders")
    def create_order(order: OrderRequest):
        exec_id = rt.go(
            order_agent,
            f"Process order: {order.quantity}x {order.item} for customer {order.customer}",
        )
        return {"execution_id": exec_id, "status": "processing"}

    @app.post("/orders/{exec_id}/approve")
    def approve_order(exec_id: str):
        conf_id = pending_approvals.get(exec_id)
        if not conf_id:
            raise HTTPException(404, "No pending approval for this execution")
        rt.signal(exec_id, conf_id, True)
        del pending_approvals[exec_id]
        return {"status": "approved", "message": "Agent will auto-resume"}

    @app.get("/orders/{exec_id}")
    def get_order(exec_id: str):
        return {"execution_id": exec_id, "pending_approval": exec_id in pending_approvals}

    @app.on_event("shutdown")
    def shutdown():
        rt.shutdown()

    print(f"\nStarting server on http://localhost:8000")
    print(f"Docs at http://localhost:8000/docs\n")
    uvicorn.run(app, host="0.0.0.0", port=8000)


if __name__ == "__main__":
    shutil.rmtree(DATA_DIR, ignore_errors=True)
    if "--mock" in sys.argv or "--serve" not in sys.argv:
        run_mock_demo()
    else:
        run_fastapi_server()
