#!/usr/bin/env python3
"""Emit only public defaults for native launchers, never the inherited environment."""
import json
import os
import sys
from build import public_defaults

if __name__ == '__main__':
    try:
        print(json.dumps(public_defaults(os.environ)))
    except (OSError, ValueError):
        sys.exit('Invalid public build defaults; check WORKSPACE_CONFIG and public build overrides.')
