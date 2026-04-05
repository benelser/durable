#!/usr/bin/env python3
"""
02 — Crash Recovery: The demo that no other framework can show.

An order processing agent that:
  1. Checks inventory (side effect #1)
  2. Charges payment (side effect #2 -- REAL MONEY)
  3. Sends confirmation email (side effect #3)

We run it TWICE with the same execution ID. The first run completes
all three steps. The second run returns cached results for all three
steps WITHOUT re-executing the tool functions.

Proof: global side-effect counters show each tool ran exactly once.

    export OPENAI_API_KEY=sk-...
    python 02_crash_recovery.py

Or without an API key:

    python 02_crash_recovery.py --mock
"""

import os
import sys
import shutil
import time

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from durable import Agent, tool

DATA_DIR = "./demo-data/crash-recovery"

# ---- Side effect counters (global, survive across agent.run() calls) ----
CHARGE_COUNT = 0
EMAIL_COUNT = 0
INVENTORY_COUNT = 0


@tool("check_inventory", description="Check if items are in stock")
def check_inventory(item_id: str, quantity: int) -> dict:
    global INVENTORY_COUNT
    INVENTORY_COUNT += 1
    print(f"    [side effect] inventory check #{INVENTORY_COUNT}: {quantity}x {item_id}")
    return {"in_stock": True, "available": 100, "unit_price": 49.99}


@tool("charge_payment", description="Charge customer's payment method")
def charge_payment(customer_id: str, amount: float) -> dict:
    global CHARGE_COUNT
    CHARGE_COUNT += 1
    print(f"    [side effect] PAYMENT #{CHARGE_COUNT}: ${amount:.2f} charged to {customer_id}")
    time.sleep(0.1)  # simulate processing
    return {"transaction_id": f"txn_{CHARGE_COUNT}", "amount": amount, "status": "charged"}


@tool("send_email", description="Send order confirmation email")
def send_email(to: str, subject: str, body: str) -> dict:
    global EMAIL_COUNT
    EMAIL_COUNT += 1
    print(f"    [side effect] EMAIL #{EMAIL_COUNT}: to={to} subject={subject}")
    return {"sent": True, "message_id": f"msg_{EMAIL_COUNT}"}


MOCK_STEP = 0

def mock_llm(messages, tools=None, model=None):
    global MOCK_STEP
    MOCK_STEP += 1

    if MOCK_STEP == 1:
        return {"tool_calls": [
            {"id": "c1", "name": "check_inventory", "arguments": {"item_id": "WIDGET-X", "quantity": 5}},
        ]}
    elif MOCK_STEP == 2:
        return {"tool_calls": [
            {"id": "c2", "name": "charge_payment", "arguments": {"customer_id": "cust-42", "amount": 249.95}},
        ]}
    elif MOCK_STEP == 3:
        return {"tool_calls": [
            {"id": "c3", "name": "send_email", "arguments": {
                "to": "alice@example.com",
                "subject": "Order ORD-789 confirmed",
                "body": "Your order of 5x WIDGET-X ($249.95) has been charged and is being shipped.",
            }},
        ]}
    else:
        return {"content": "Order ORD-789 processed successfully. 5x WIDGET-X, $249.95 charged, confirmation sent to alice@example.com."}


def make_agent():
    agent = Agent(
        DATA_DIR,
        system_prompt=(
            "You are an order processing agent. Process orders by:\n"
            "1. Checking inventory\n"
            "2. Charging payment\n"
            "3. Sending confirmation email\n"
            "Call tools in this exact order, then summarize the result."
        ),
    )
    agent.add_tool(check_inventory)
    agent.add_tool(charge_payment)
    agent.add_tool(send_email)

    if "--mock" in sys.argv:
        global MOCK_STEP
        MOCK_STEP = 0
        agent.set_llm(mock_llm)
    else:
        from durable.providers import OpenAI
        agent.set_llm(OpenAI())

    return agent


def main():
    shutil.rmtree(DATA_DIR, ignore_errors=True)

    print("=" * 64)
    print("  Crash Recovery Demo: Exactly-Once Side Effects")
    print("=" * 64)

    # ---- FIRST RUN: execute everything ----
    print("\n--- Run 1: Full execution ---\n")

    agent = make_agent()
    response = agent.run("Process order: 5x WIDGET-X for customer cust-42, email alice@example.com")
    exec_id = response.execution_id
    print(f"\n  Result: {response.text}")
    print(f"  Execution ID: {exec_id}")
    agent.close()

    print(f"\n  Side effects after run 1:")
    print(f"    Inventory checks: {INVENTORY_COUNT}")
    print(f"    Payments charged: {CHARGE_COUNT}")
    print(f"    Emails sent:      {EMAIL_COUNT}")

    # ---- SECOND RUN: simulate crash recovery ----
    print("\n--- Run 2: Resume (simulating restart after crash) ---\n")
    print("  The agent is re-created from scratch. All tool functions are")
    print("  registered again. But the event log remembers what already ran.\n")

    agent2 = make_agent()
    response2 = agent2.run(
        "Process order: 5x WIDGET-X for customer cust-42, email alice@example.com",
        execution_id=exec_id,
    )
    print(f"\n  Result: {response2.text}")
    agent2.close()

    print(f"\n  Side effects after run 2:")
    print(f"    Inventory checks: {INVENTORY_COUNT}")
    print(f"    Payments charged: {CHARGE_COUNT}")
    print(f"    Emails sent:      {EMAIL_COUNT}")

    # ---- VERIFICATION ----
    print(f"\n{'=' * 64}")
    print("  EXACTLY-ONCE VERIFICATION")
    print(f"{'=' * 64}")

    all_once = (INVENTORY_COUNT == 1 and CHARGE_COUNT == 1 and EMAIL_COUNT == 1)
    for name, count in [("Inventory checks", INVENTORY_COUNT),
                        ("Payments charged", CHARGE_COUNT),
                        ("Emails sent", EMAIL_COUNT)]:
        status = "PASS" if count == 1 else "FAIL"
        print(f"  [{status}] {name}: {count} (expected: 1)")

    if all_once:
        print(f"\n  All side effects executed exactly once.")
        print(f"  The customer was charged $249.95 once. Not zero. Not twice.")
    else:
        print(f"\n  FAILURE: Side effects were duplicated or lost!")

    print(f"\n  Try inspecting the execution:")
    print(f"    durable status --data-dir {DATA_DIR}")
    print(f"    durable steps {exec_id} --data-dir {DATA_DIR}")
    print()

    shutil.rmtree(DATA_DIR, ignore_errors=True)


if __name__ == "__main__":
    main()
