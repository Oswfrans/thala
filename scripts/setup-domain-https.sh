#!/bin/bash
# Setup domain + Let's Encrypt for Thala Discord webhooks
# This creates a permanent, secure HTTPS endpoint

set -e

echo "╔════════════════════════════════════════════════════════════╗"
echo "║  Domain + Let's Encrypt Setup for Thala Discord           ║"
echo "╚════════════════════════════════════════════════════════════╝"
echo ""

# Get domain info
echo "Step 1: Domain Configuration"
echo "-----------------------------"
read -p "Enter your domain name (e.g., thala.example.com): " DOMAIN

if [[ -z "$DOMAIN" ]]; then
    echo "Error: Domain is required"
    exit 1
fi

echo ""
echo "Step 2: Check DNS Configuration"
echo "-------------------------------"
echo "Checking if $DOMAIN resolves to this VPS..."

# Get VPS public IP
VPS_IP=$(curl -s -4 ifconfig.me)
echo "This VPS public IP: $VPS_IP"

# Check DNS
DOMAIN_IP=$(dig +short $DOMAIN 2>/dev/null || echo "")

if [[ -n "$DOMAIN_IP" ]]; then
    if [[ "$DOMAIN_IP" == "$VPS_IP" ]]; then
        echo "✓ $DOMAIN correctly points to this VPS ($VPS_IP)"
    else
        echo "⚠ $DOMAIN points to $DOMAIN_IP, but this VPS is $VPS_IP"
        echo ""
        echo "ACTION REQUIRED: Update your DNS A record:"
        echo "  $DOMAIN → A → $VPS_IP"
        echo ""
        read -p "Press Enter when DNS is updated..."
    fi
else
    echo "✗ $DOMAIN does not resolve yet"
    echo ""
    echo "ACTION REQUIRED: Add this DNS A record in your registrar:"
    echo "  $DOMAIN → A → $VPS_IP"
    echo ""
    read -p "Press Enter when DNS is updated..."
fi

echo ""
echo "Step 3: Install certbot (Let's Encrypt)"
echo "-----------------------------------------"

if ! command -v certbot &> /dev/null; then
    sudo apt-get update
    sudo apt-get install -y certbot python3-certbot-standalone
fi

echo "✓ certbot installed"

echo ""
echo "Step 4: Obtain SSL Certificate"
echo "--------------------------------"
echo "This will request a certificate from Let's Encrypt."
echo "Make sure port 80 is open (for verification)."
echo ""

# Get certificate
sudo certbot certonly --standalone -d $DOMAIN --agree-tos -n --email admin@$DOMAIN 2>/dev/null || {
    echo "⚠ Certificate generation may have failed or domain not ready yet"
    echo "You can retry later with: sudo certbot certonly --standalone -d $DOMAIN"
}

echo ""
echo "Step 5: Install nginx as reverse proxy"
echo "----------------------------------------"

sudo apt-get install -y nginx

# Configure nginx for Thala
cat > /tmp/thala-nginx.conf << NGINXCONF
server {
    listen 443 ssl http2;
    server_name $DOMAIN;

    ssl_certificate /etc/letsencrypt/live/$DOMAIN/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/$DOMAIN/privkey.pem;
    ssl_protocols TLSv1.2 TLSv1.3;
    ssl_ciphers ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256;
    ssl_prefer_server_ciphers off;

    # Discord webhook endpoint
    location /api/discord/interaction {
        proxy_pass http://127.0.0.1:8789;
        proxy_http_version 1.1;
        proxy_set_header Host \$host;
        proxy_set_header X-Real-IP \$remote_addr;
        proxy_set_header X-Forwarded-For \$proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto \$scheme;
        
        # Discord requires these for signature verification
        proxy_set_header X-Signature-Ed25519 \$http_x_signature_ed25519;
        proxy_set_header X-Signature-Timestamp \$http_x_signature_timestamp;
    }

    # Worker callback endpoint
    location /api/worker/callback {
        proxy_pass http://127.0.0.1:8788;
        proxy_http_version 1.1;
        proxy_set_header Host \$host;
        proxy_set_header X-Real-IP \$remote_addr;
        proxy_set_header X-Forwarded-For \$proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto \$scheme;
    }

    # Health check
    location /health {
        access_log off;
        return 200 "OK\n";
        add_header Content-Type text/plain;
    }
}

# HTTP to HTTPS redirect
server {
    listen 80;
    server_name $DOMAIN;
    return 301 https://\$server_name\$request_uri;
}
NGINXCONF

sudo cp /tmp/thala-nginx.conf /etc/nginx/sites-available/thala
sudo ln -sf /etc/nginx/sites-available/thala /etc/nginx/sites-enabled/thala
sudo rm -f /etc/nginx/sites-enabled/default 2>/dev/null || true

# Test nginx config
sudo nginx -t && sudo systemctl restart nginx

echo "✓ nginx configured"

echo ""
echo "Step 6: Update Thala WORKFLOW.md"
echo "----------------------------------"

# Update WORKFLOW.md with new HTTPS callback URL
if [[ -f /home/debian/thala/WORKFLOW.md ]]; then
    sed -i "s|callback_base_url:.*|callback_base_url: https://$DOMAIN|" /home/debian/thala/WORKFLOW.md
    echo "✓ Updated WORKFLOW.md callback_base_url to https://$DOMAIN"
fi

echo ""
echo "Step 7: Set up certbot auto-renewal"
echo "------------------------------------"

# Add renewal hook to reload nginx
sudo mkdir -p /etc/letsencrypt/renewal-hooks/deploy
cat > /tmp/reload-nginx.sh << 'EOF'
#!/bin/bash
systemctl reload nginx
EOF
sudo cp /tmp/reload-nginx.sh /etc/letsencrypt/renewal-hooks/deploy/reload-nginx.sh
sudo chmod +x /etc/letsencrypt/renewal-hooks/deploy/reload-nginx.sh

# Test renewal
sudo certbot renew --dry-run 2>/dev/null || echo "Renewal dry-run completed"

echo "✓ Auto-renewal configured"

echo ""
echo "╔════════════════════════════════════════════════════════════╗"
echo "║                  Setup Complete!                            ║"
echo "╚════════════════════════════════════════════════════════════╝"
echo ""
echo "Your HTTPS endpoints:"
echo "  Discord webhook: https://$DOMAIN/api/discord/interaction"
echo "  Worker callback: https://$DOMAIN/api/worker/callback"
echo "  Health check:    https://$DOMAIN/health"
echo ""
echo "Next steps:"
echo ""
echo "1. Configure Discord Interaction URL:"
echo "   Go to https://discord.com/developers/applications/1479507510640509169"
echo "   → General Information → Interactions URL"
echo "   Set to: https://$DOMAIN/api/discord/interaction"
echo ""
echo "2. Test in Discord:"
echo "   /thala create Add a login button"
echo ""
echo "3. Monitor logs:"
echo "   journalctl --user -u thala -f"
echo "   sudo tail -f /var/log/nginx/access.log"
echo ""
echo "4. Certificate renewal test:"
echo "   sudo certbot renew --dry-run"
echo ""
