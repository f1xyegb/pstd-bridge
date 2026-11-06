# pstd-bridge

The mailbox between your own Claude and [pstd](https://f1xyegb.github.io/pstd/). Claude plans posts on a schedule, this server holds them, pstd takes them into the right projects and networks.

It runs on your server. Your posts and keys stay there.

```sh
curl -fsSL https://f1xyegb.github.io/pstd/bridge.sh | sudo sh
```

Linux with systemd. It asks for a domain and an image key, either can be skipped, and ends on a QR for pstd.

```
pstd-bridge setup     set this server up
pstd-bridge serve     run the server
pstd-bridge connect   show the QR for pstd and the line for Claude
pstd-bridge key       set the image generator's key
```

Claude connects over MCP at `/mcp`: `pstd_plan`, `pstd_generate_image`, `pstd_add_post`, `pstd_queue`.
