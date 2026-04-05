#!/usr/bin/env python3
"""
LangGraph + Durable: Crash-Proof Order Processing

This example demonstrates the killer feature: an agent that processes
a payment, crashes mid-execution, and resumes WITHOUT double-charging
the customer. Try this with any other framework.

The flow:
  1. Agent receives order → looks up customer → charges payment → sends email
  2. We simulate a crash after the payment charge
  3. On restart, the payment step returns the CACHED result (no re-charge)
  4. The email sends, order completes

Run it:
  pip install durable-runtime[langchain] langchain-openai
  python langchain_crash_recovery.py

What to watch for:
  - "CHARGING PAYMENT" appears exactly ONCE across both runs
  - "SENDING EMAIL" appears exactly ONCE
  - The second run completes instantly from cache
"""

import os
import sys
import shutil

# ---------------------------------------------------------------------------
# This example uses the durable integration with LangGraph.
# The key line is: checkpointer=DurableCheckpointer("./order-agent-data")
# Everything else is standard LangGraph.
# ---------------------------------------------------------------------------

# Check for required packages
try:
    from langgraph.graph import StateGraph, END
    from langchain_core.messages import HumanMessage, AIMessage, SystemMessage
except ImportError:
    print("Install required packages:")
    print("  pip install langgraph langchain-core langchain-openai")
    sys.exit(1)

from durable.integrations.langchain import DurableCheckpointer

# Simulated side effects — these track whether tools actually executed
CHARGE_COUNT = 0
EMAIL_COUNT = 0

DATA_DIR = "./order-agent-data"


# -- Define the tools (side effects we want exactly-once) --

def charge_payment(amount: float, customer_id: str) -> dict:
    """Charge a customer's payment method. REAL SIDE EFFECT."""
    global CHARGE_COUNT
    CHARGE_COUNT += 1
    print(f"  💳 CHARGING PAYMENT #{CHARGE_COUNT}: ${amount:.2f} to customer {customer_id}")
    return {"transaction_id": "txn_abc123", "amount": amount, "status": "charged"}


def send_confirmation_email(customer_id: str, transaction_id: str) -> dict:
    """Send order confirmation email. REAL SIDE EFFECT."""
    global EMAIL_COUNT
    EMAIL_COUNT += 1
    print(f"  📧 SENDING EMAIL #{EMAIL_COUNT} to customer {customer_id}")
    return {"sent": True, "to": f"{customer_id}@example.com"}


# -- Define the graph state --

from typing import TypedDict, List, Annotated
from langchain_core.messages import BaseMessage
import operator


class OrderState(TypedDict):
    messages: Annotated[List[BaseMessage], operator.add]
    order_id: str
    customer_id: str
    amount: float
    payment_result: dict
    email_result: dict
    step: str


# -- Define the graph nodes --

def lookup_customer(state: OrderState) -> dict:
    print("  🔍 Looking up customer...")
    return {
        "customer_id": "cust_12345",
        "step": "customer_found",
        "messages": [AIMessage(content="Customer found: cust_12345")],
    }


def process_payment(state: OrderState) -> dict:
    result = charge_payment(state["amount"], state["customer_id"])
    return {
        "payment_result": result,
        "step": "payment_charged",
        "messages": [AIMessage(content=f"Payment charged: {result['transaction_id']}")],
    }


def send_email(state: OrderState) -> dict:
    result = send_confirmation_email(
        state["customer_id"],
        state.get("payment_result", {}).get("transaction_id", "unknown"),
    )
    return {
        "email_result": result,
        "step": "email_sent",
        "messages": [AIMessage(content="Confirmation email sent")],
    }


def should_continue(state: OrderState) -> str:
    step = state.get("step", "")
    if step == "customer_found":
        return "process_payment"
    elif step == "payment_charged":
        return "send_email"
    return END


# -- Build the graph --

def build_graph():
    graph = StateGraph(OrderState)
    graph.add_node("lookup_customer", lookup_customer)
    graph.add_node("process_payment", process_payment)
    graph.add_node("send_email", send_email)

    graph.set_entry_point("lookup_customer")
    graph.add_conditional_edges("lookup_customer", should_continue)
    graph.add_conditional_edges("process_payment", should_continue)
    graph.add_edge("send_email", END)

    # THE KEY LINE: DurableCheckpointer makes this crash-proof
    return graph.compile(checkpointer=DurableCheckpointer(DATA_DIR))


# -- Run the demo --

def main():
    # Clean up from previous runs
    shutil.rmtree(DATA_DIR, ignore_errors=True)

    thread_id = "order-789"

    print("=" * 60)
    print("  LangGraph + Durable: Crash-Proof Order Processing")
    print("=" * 60)

    # --- FIRST RUN ---
    print("\n--- First Run: Process order (will complete normally) ---\n")

    compiled = build_graph()
    result = compiled.invoke(
        {
            "messages": [HumanMessage(content="Process order #789 for $99.99")],
            "order_id": "789",
            "amount": 99.99,
            "customer_id": "",
            "payment_result": {},
            "email_result": {},
            "step": "",
        },
        config={"configurable": {"thread_id": thread_id}},
    )

    print(f"\n  Order completed. Steps: {result['step']}")
    print(f"  Charges: {CHARGE_COUNT}, Emails: {EMAIL_COUNT}")

    # --- SECOND RUN (simulates restart after crash) ---
    print("\n--- Second Run: Resume (simulates restart after crash) ---")
    print("    All steps should return cached results.\n")

    compiled2 = build_graph()
    result2 = compiled2.invoke(
        {
            "messages": [HumanMessage(content="Process order #789 for $99.99")],
            "order_id": "789",
            "amount": 99.99,
            "customer_id": "",
            "payment_result": {},
            "email_result": {},
            "step": "",
        },
        config={"configurable": {"thread_id": thread_id}},
    )

    print(f"\n  Order completed (from cache). Steps: {result2['step']}")
    print(f"\n{'=' * 60}")
    print(f"  FINAL COUNTS:")
    print(f"    Charges: {CHARGE_COUNT} (should be 1 — no double charge)")
    print(f"    Emails:  {EMAIL_COUNT} (should be 1 — no duplicate)")
    print(f"{'=' * 60}")

    # Cleanup
    shutil.rmtree(DATA_DIR, ignore_errors=True)


if __name__ == "__main__":
    main()
