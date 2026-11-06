# pstd-bridge

The mailbox between your own Claude and [pstd](https://f1xyegb.github.io/pstd/). Claude plans posts on a schedule, this server holds them, pstd takes them into the right projects and networks.

It runs on your server. Your posts and keys stay there.

```sh
curl -fsSL https://f1xyegb.github.io/pstd/bridge.sh | sudo sh -s -- --auto
```

Linux with systemd. Add `--domain example.com` if the server has one; a website on it stays as it is. It ends on a QR for pstd. pstd shows these steps with your details filled in.

```
pstd-bridge setup     set this server up
pstd-bridge serve     run the server
pstd-bridge connect   show the QR for pstd and the line for Claude
pstd-bridge key       set the image generator's key
```

Claude connects over MCP at `/mcp`: `pstd_plan`, `pstd_generate_image`, `pstd_add_post`, `pstd_queue`. A picture made on Claude's own machine goes up as a raw file, and the image key is then not needed:

```sh
curl -H "Authorization: Bearer $TOKEN" --data-binary @post.png https://your-server/mcp/media
```
