#!/usr/bin/env python3
"""
05 — Contract Enforcement: Business rules the LLM cannot violate.

Contracts are invariant checks that run BEFORE a tool executes. If a
contract is violated, execution suspends for human review -- the tool
never runs.

This is not prompt engineering. The LLM can hallucinate, ignore
instructions, or be manipulated by prompt injection. Contracts are
code-level guardrails that execute in the runtime, not the model.

    export OPENAI_API_KEY=sk-...
    python 05_contract_enforcement.py

Or:
    python 05_contract_enforcement.py --mock
"""

import os
import sys
import shutil

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from durable import Agent, tool

DATA_DIR = "./demo-data/contracts"

CHARGE_COUNT = 0
DELETE_COUNT = 0


@tool("charge_payment", description="Charge a customer's payment method")
def charge_payment(customer_id: str, amount: float) -> dict:
    global CHARGE_COUNT
    CHARGE_COUNT += 1
    print(f"    [charge] #{CHARGE_COUNT}: ${amount:.2f} to {customer_id}")
    return {"transaction_id": f"txn_{CHARGE_COUNT}", "amount": amount, "status": "charged"}


@tool("delete_account", description="Permanently delete a customer account")
def delete_account(customer_id: str, reason: str) -> dict:
    global DELETE_COUNT
    DELETE_COUNT += 1
    print(f"    [delete] #{DELETE_COUNT}: {customer_id} ({reason})")
    return {"deleted": True, "customer_id": customer_id}


@tool("send_refund", description="Issue a refund to a customer")
def send_refund(customer_id: str, amount: float) -> dict:
    print(f"    [refund] ${amount:.2f} to {customer_id}")
    return {"refund_id": "ref_001", "amount": amount, "status": "processed"}


MOCK_STEP = 0

def mock_llm(messages, tools=None, model=None):
    """Mock LLM that tries to violate contracts."""
    global MOCK_STEP
    MOCK_STEP += 1

    if MOCK_STEP == 1:
        # Try a reasonable charge first
        return {"tool_calls": [
            {"id": "c1", "name": "charge_payment", "arguments": {"customer_id": "cust-42", "amount": 99.99}},
        ]}
    elif MOCK_STEP == 2:
        # Now try an outrageous charge -- contract will block this
        return {"tool_calls": [
            {"id": "c2", "name": "charge_payment", "arguments": {"customer_id": "cust-42", "amount": 50000.00}},
        ]}
    elif MOCK_STEP == 3:
        # After suspension, try a refund instead (safe)
        return {"tool_calls": [
            {"id": "c3", "name": "send_refund", "arguments": {"customer_id": "cust-42", "amount": 99.99}},
        ]}
    else:
        return {"content": "Charged $99.99, then issued a full refund of $99.99 to cust-42."}


def main():
    shutil.rmtree(DATA_DIR, ignore_errors=True)

    print("=" * 64)
    print("  Contract Enforcement: Code-Level Guardrails")
    print("=" * 64)

    agent = Agent(
        DATA_DIR,
        system_prompt=(
            "You are a customer service agent. Process the following:\n"
            "1. Charge $99.99 to cust-42\n"
            "2. Then charge $50,000 to cust-42\n"
            "3. If the second charge fails, issue a refund for the first"
        ),
    )
    agent.add_tool(charge_payment)
    agent.add_tool(delete_account)
    agent.add_tool(send_refund)

    # Contract 1: No charges over $10,000
    @agent.contract("max-charge-limit")
    def check_charge_limit(step_name, args):
        if "charge_payment" in step_name:
            amount = args.get("amount", 0)
            if isinstance(amount, (int, float)) and amount > 10_000:
                raise ValueError(
                    f"charge of ${amount:,.2f} exceeds $10,000 limit -- "
                    f"requires VP approval"
                )

    # Contract 2: Account deletion is never allowed via agent
    @agent.contract("no-account-deletion")
    def block_deletion(step_name, args):
        if "delete_account" in step_name:
            raise ValueError("account deletion is prohibited via automated agents")

    if "--mock" in sys.argv:
        agent.set_llm(mock_llm)
    else:
        from durable.providers import OpenAI
        agent.set_llm(OpenAI())

    # ---- Run: Agent tries to violate contracts ----
    print("\n--- Agent attempts operations ---\n")
    print("  Contracts:")
    print("    1. No charges over $10,000")
    print("    2. Account deletion is always blocked\n")

    response = agent.run("Process the customer operations as instructed")
    exec_id = response.execution_id

    print(f"\n  Status: {response.status.value}")

    if response.is_suspended:
        reason = response.suspend_reason
        print(f"  Suspended: {reason.type}")
        if reason.contract_name:
            print(f"    Contract: {reason.contract_name}")
        print(f"\n  The $50,000 charge was BLOCKED before the tool ran.")
        print(f"  Charges that actually executed: {CHARGE_COUNT}")
        print(f"  The $99.99 charge went through (under $10k limit).")

        # Resume after the contract violation (agent should adapt)
        print("\n--- Resume: Agent adapts after contract violation ---\n")
        response2 = agent.resume(exec_id)
        if response2.text:
            print(f"  Agent: {response2.text}")
        print(f"  Status: {response2.status.value}")
    else:
        print(f"  Agent: {response.text}")

    # ---- Verification ----
    print(f"\n{'=' * 64}")
    print("  VERIFICATION")
    print(f"{'=' * 64}")
    print(f"  Charges executed: {CHARGE_COUNT}")
    print(f"  Accounts deleted: {DELETE_COUNT}")
    print()
    print("  Contracts are NOT prompt engineering. They are code-level checks")
    print("  that the LLM cannot circumvent through hallucination, jailbreaking,")
    print("  or prompt injection. The tool function never executes if the")
    print("  contract fails.")
    print()

    agent.close()
    shutil.rmtree(DATA_DIR, ignore_errors=True)


if __name__ == "__main__":
    main()
