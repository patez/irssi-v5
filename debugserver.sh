#bash
HOSTNAME=$(tailscale status --json | python3 -c "import sys,json; d=json.load(sys.stdin); print(d['Self']['DNSName'].rstrip('.'))")
caddy reverse-proxy --from $HOSTNAME --to localhost:3001
