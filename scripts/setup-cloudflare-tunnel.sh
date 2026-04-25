#!/bin/bash
# Setup Cloudflare Tunnel for Thala Discord webhook
# This creates a secure tunnel from Cloudflare edge to your local Discord server

set -e

echo "╔════════════════════════════════════════════════════════════╗"
echo "║     Cloudflare Tunnel Setup for Thala Discord              ║"
echo "╚════════════════════════════════════════════════════════════╝"
echo ""

# Check if already authenticated
if [[ ! -f ~/.cloudflared/cert.pem ]]; then
    echo "Step 1: Authenticate with Cloudflare"
    echo "-------------------------------------"
    echo "This will open a browser to authenticate cloudflared."
    echo "After authenticating, come back here."
    echo ""
    read -p "Press Enter to continue..."
    
    cloudflared tunnel login
    
    echo ""
    echo "✓ Authenticated successfully!"
else
    echo "✓ Already authenticated with Cloudflare"
fi

echo ""
echo "Step 2: Create Tunnel"
echo "---------------------"

# Check if thala-discord tunnel exists
if cloudflared tunnel list | grep -q "thala-discord"; then
    echo "✓ Tunnel 'thala-discord' already exists"
    TUNNEL_ID=$(cloudflared tunnel list | grep "thala-discord" | awk '{print $1}')
else
    echo "Creating tunnel 'thala-discord'..."
    cloudflared tunnel create thala-discord
    TUNNEL_ID=$(cloudflared tunnel list | grep "thala-discord" | awk '{print $1}')
    echo "✓ Tunnel created: $TUNNEL_ID"
fi

echo ""
echo "Step 3: Configure Routing"
echo "-------------------------"

# Get the tunnel credentials file
CREDS_FILE=$(ls ~/.cloudflared/*.json | head -1)

# Create config file
mkdir -p ~/.cloudflared
cat > ~/.cloudflared/config.yml << EOF
tunnel: ${TUNNEL_ID}
credentials-file: ${CREDS_FILE}

ingress:
  # Discord webhook endpoint
  - hostname: thala-discord.YOUR_DOMAIN.com
    service: http://localhost:8789
  # Worker callback endpoint  
  - hostname: thala-callback.YOUR_DOMAIN.com
    service: http://localhost:8788
  # Default catch-all
  - service: http_status:404
EOF

echo "✓ Config file created at ~/.cloudflared/config.yml"
echo ""
echo "⚠ IMPORTANT: You need to set up DNS records in Cloudflare dashboard:"
echo "   1. Go to https://dash.cloudflare.com"
echo "   2. Select your domain"
echo "   3. Add CNAME records:"
echo "      - thala-discord → ${TUNNEL_ID}.cfargotunnel.com"
echo "      - thala-callback → ${TUNNEL_ID}.cfargotunnel.com"
echo ""

# Ask for domain
read -p "Enter your Cloudflare domain (e.g., example.com): " DOMAIN

if [[ -n "$DOMAIN" ]]; then
    # Update config with actual domain
    sed -i "s/YOUR_DOMAIN.com/${DOMAIN}/g" ~/.cloudflared/config.yml
    
    echo ""
    echo "Attempting to create DNS records automatically..."
    
    # Create DNS records via cloudflared
    cloudflared tunnel route dns thala-discord "thala-discord.${DOMAIN}" 2>/dev/null || echo "⚠ Could not auto-create DNS. Please create manually in dashboard."
    cloudflared tunnel route dns thala-discord "thala-callback.${DOMAIN}" 2>/dev/null || echo "⚠ Could not auto-create DNS. Please create manually in dashboard."
fi

echo ""
echo "Step 4: Create Systemd Service for Tunnel"
echo "-------------------------------------------"

sudo tee /etc/systemd/system/cloudflared-thala.service > /dev/null << 'EOF'
[Unit]
Description=Cloudflare Tunnel for Thala
After=network.target

[Service]
Type=simple
User=debian
ExecStart=/usr/bin/cloudflared tunnel --config /home/debian/.cloudflared/config.yml run
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable cloudflared-thala.service

echo "✓ Systemd service created: cloudflared-thala.service"
echo ""
echo "╔════════════════════════════════════════════════════════════╗"
echo "║                     Setup Complete!                        ║"
echo "╚════════════════════════════════════════════════════════════╝"
echo ""
echo "Next steps:"
echo ""
echo "1. Start the tunnel:"
echo "   sudo systemctl start cloudflared-thala"
echo ""
echo "2. Get your tunnel URL:"
if [[ -n "$DOMAIN" ]]; then
    echo "   Discord webhook: https://thala-discord.${DOMAIN}"
    echo "   Worker callback: https://thala-callback.${DOMAIN}"
else
    echo "   Check: cloudflared tunnel list"
fi
echo ""
echo "3. Configure Discord Interaction URL:"
echo "   Go to https://discord.com/developers/applications/YOUR_APP_ID"
echo "   → General Information → Interactions URL"
echo "   Set to: https://thala-discord.YOUR_DOMAIN.com/api/discord/interaction"
echo ""
echo "4. Update Thala callback_base_url in WORKFLOW.md to use HTTPS"
echo ""
echo "5. Monitor tunnel logs:"
echo "   sudo journalctl -u cloudflared-thala -f"
echo ""
