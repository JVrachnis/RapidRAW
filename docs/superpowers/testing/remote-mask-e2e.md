# Remote AI masking — manual E2E

Prereqs:
- Gateway running: locally (`cd ~/Apps/RapidRawFork/rr-ai-gateway && GATEWAY_TOOLS_DIR=~/comfy .venv/bin/python main.py` — port 5000, COMFY_PORT=8188 env if using local ComfyUI) or on inferno via the systemd unit (deploy/rr-ai-gateway.service).
- RapidRAW settings → AI provider Self-Hosted → backend address `127.0.0.1:5000` (or `inferno:5000`).
- Dev build: `npm run tauri dev`. Note: building the Tauri app needs GTK/WebKit devel packages; either `sudo dnf install dbus-devel gtk3-devel webkit2gtk4.1-devel libsoup3-devel` or export the pkg-config sysroot used during development.
- Local-machine note: BiRefNet inside ComfyUI OOMs on the 3060 Ti — use **SAM3 backend** (Settings → Segmentation backend) for prompt-mode tests locally; SAM2/mask_hq prompt mode needs inferno (or ComfyUI on the 16 GB card).

For each payload mode (Settings → Mask payload = TIFF, then RAW), on one landscape AND one portrait .ARW:

- [ ] "AI (Remote)" appears in the mask creation list only when the gateway is reachable
- [ ] Prompt mode (SAM3): query "the main subject." → mask appears, hugs edges at 100% zoom
- [ ] Prompt + Multi-rep: both GPUs active during generation (watch `nvidia-smi`)
- [ ] Prompt + Agentic: gateway log shows mask_agentic (LLM/VLM iterations)
- [ ] Points mode: 1 fg click on subject → subject masked; +1 Alt-click bg refines; **portrait image: mask lands on the clicked object, not rotated 90°**
- [ ] Paint mode: rough scribble over subject → mask snaps to subject within region; portrait image aligned
- [ ] Preset: Subject / Sky / Foreground each return plausible masks (needs mask_hq → inferno or big-GPU ComfyUI)
- [ ] Queue: submit a multirep job, then a second mask → second shows queue position
- [ ] Cancel mid-run → job cancelled on gateway (log), UI returns to idle
- [ ] RAW payload shows best-effort alignment note; TIFF does not
- [ ] Mask adjustments (exposure -1) apply only inside mask; invert works
- [ ] .rrdata round-trip: saved file reopens with remote mask intact
- [ ] Stock inpaint (generative edit) still works against the same gateway address
- [ ] Found-labels caption appears after SAM3 prompt generation
