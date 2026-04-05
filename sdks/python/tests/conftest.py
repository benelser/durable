# Disable asyncio test mode for e2e tests that use threading
import os
import sys

# Ensure the SDK is importable
sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
