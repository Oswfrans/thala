#!/bin/bash
# Quick setup for Discord + Modal using the onboard wizard
# This script makes the old manual setup obsolete

echo "╔════════════════════════════════════════════════════════════╗"
echo "║     Thala Discord + Modal Setup via Onboard Wizard         ║"
echo "╚════════════════════════════════════════════════════════════╝"
echo ""

# Check if Thala is built
if [[ ! -f "target/release/thala" ]]; then
    echo "Building Thala..."
    cargo build --release
fi

# Run the onboard wizard
echo ""
echo "Starting onboard wizard..."
echo "This will collect all credentials and generate WORKFLOW.md"
echo ""

./target/release/thala onboard
