#!/usr/bin/env python3
"""
03 — Human-in-the-Loop: Confirmation gates for dangerous actions.

The agent has a tool marked `requires_confirmation=True`. When the LLM
tries to use it, execution SUSPENDS and waits for human approval. The
human can approve or reject with a reason.

This is not a client-side hack -- the suspension is durable. The process
can crash between "agent requests approval" and "human approves" and
nothing is lost. Resume picks up exactly where it left off.

    export OPENAI_API_KEY=sk-...
    python 03_human_in_the_loop.py

Or:
    python 03_human_in_the_loop.py --mock
"""

import os
import sys
import shutil
import time

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from durable import Agent, tool

DATA_DIR = "./demo-data/human-in-the-loop"

TRANSFER_COUNT = 0


@tool("check_balance", description="Check account balance")
def check_balance(account_id: str) -> dict:
    print(f"    [check_balance] Looking up {account_id}")
    return {"account_id": account_id, "balance": 15_420.00, "currency": "USD"}


@tool(
    "transfer_funds",
    description="Transfer funds between accounts. Requires human approval.",
    requires_confirmation=True,
)
def transfer_funds(from_account: str, to_account: str, amount: float) -> dict:
    global TRANSFER_COUNT
    TRANSFER_COUNT += 1
    print(f"    [transfer_funds] #{TRANSFER_COUNT}: ${amount:.2f} from {from_account} to {to_account}")
    return {"transfer_id": f"xfr_{TRANSFER_COUNT}", "amount": amount, "status": "completed"}


MOCK_STEP = 0

def mock_llm(messages, tools=None, model=None):
    global MOCK_STEP
    MOCK_STEP += 1

    if MOCK_STEP == 1:
        return {"tool_calls": [
            {"id": "c1", "name": "check_balance", "arguments": {"account_id": "acct-001"}},
        ]}
    elif MOCK_STEP == 2:
        return {"tool_calls": [
            {"id": "c2", "name": "transfer_funds", "arguments": {
                "from_account": "acct-001",
                "to_account": "acct-099",
                "amount": 5000.00,
            }},
        ]}
    else:
        return {"content": "Transfer of $5,000 from acct-001 to acct-099 completed successfully. Remaining balance: $10,420."}


def main():
    shutil.rmtree(DATA_DIR, ignore_errors=True)

    print("=" * 64)
    print("  Human-in-the-Loop: Confirmation Gates")
    print("=" * 64)

    agent = Agent(
        DATA_DIR,
        system_prompt=(
            "You are a banking assistant. When asked to transfer money:\n"
            "1. Check the source account balance\n"
            "2. Transfer the funds (this requires human approval)\n"
            "3. Report the result"
        ),
    )
    agent.add_tool(check_balance)
    agent.add_tool(transfer_funds)

    if "--mock" in sys.argv:
        agent.set_llm(mock_llm)
    else:
        from durable.providers import OpenAI
        agent.set_llm(OpenAI())

    # ---- Step 1: Agent runs and hits the confirmation gate ----
    print("\n--- Step 1: Agent encounters a dangerous action ---\n")

    response = agent.run("Transfer $5,000 from acct-001 to acct-099")
    exec_id = response.execution_id

    if response.is_suspended:
        reason = response.suspend_reason
        print(f"  Agent SUSPENDED: {reason.type}")
        print(f"    Tool: {reason.tool_name}")
        print(f"    Confirmation ID: {reason.confirmation_id}")
        print(f"    Transfers so far: {TRANSFER_COUNT} (should be 0)")
        print()
        print("  The agent is paused. In production, this could be a Slack")
        print("  message, an email, a dashboard notification, etc.")
        print("  The process could crash here and nothing would be lost.")

        # ---- Step 2: Human approves ----
        print("\n--- Step 2: Human approves the transfer ---\n")

        time.sleep(0.5)  # dramatic pause
        print("  Manager reviews and approves...")
        agent.approve(exec_id, reason.confirmation_id)

        # ---- Step 3: Resume execution ----
        print("\n--- Step 3: Agent resumes ---\n")

        response2 = agent.resume(exec_id)
        print(f"  Agent: {response2.text}")
        print(f"  Status: {response2.status.value}")
        print(f"  Transfers executed: {TRANSFER_COUNT} (should be 1)")
    else:
        print(f"  Agent completed without suspension: {response.text}")
        print(f"  (The LLM may not have used the confirmation-gated tool)")

    # ---- Verification ----
    print(f"\n{'=' * 64}")
    print("  VERIFICATION")
    print(f"{'=' * 64}")
    print(f"  Transfer executed: {'YES' if TRANSFER_COUNT == 1 else 'NO'} ({TRANSFER_COUNT} time(s))")
    print(f"  The dangerous action only ran AFTER human approval.")
    print()

    agent.close()
    shutil.rmtree(DATA_DIR, ignore_errors=True)


if __name__ == "__main__":
    main()
