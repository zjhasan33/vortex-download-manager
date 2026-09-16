import { formatBytes } from "./format";

export class SpeedChart {
  private canvas: HTMLCanvasElement;
  private points: number[] = [];
  private ctx: CanvasRenderingContext2D;
  private max = 1;
  private raf = 0;

  constructor(canvas: HTMLCanvasElement) {
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d")!;
  }

  push(speed: number) {
    this.points.push(speed);
    if (this.points.length > 180) this.points.shift();
    let m = speed;
    for (const p of this.points) if (p > m) m = p;
    this.max = Math.max(m, 1) * 1.15;
    if (!this.raf) {
      this.raf = requestAnimationFrame(() => {
        this.raf = 0;
        this.draw();
      });
    }
  }

  reset() {
    this.points = [];
    this.max = 1;
    this.draw();
  }

  draw() {
    const dpr = window.devicePixelRatio || 1;
    const w = this.canvas.clientWidth;
    const h = this.canvas.clientHeight;
    if (w === 0 || h === 0) return;
    if (this.canvas.width !== w * dpr || this.canvas.height !== h * dpr) {
      this.canvas.width = w * dpr;
      this.canvas.height = h * dpr;
    }
    const ctx = this.ctx;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);

    const grad = ctx.createLinearGradient(0, 0, w, 0);
    grad.addColorStop(0, "#22d3ee");
    grad.addColorStop(0.5, "#818cf8");
    grad.addColorStop(1, "#c084fc");
    ctx.strokeStyle = grad;
    ctx.lineWidth = 2;
    ctx.lineJoin = "round";
    ctx.lineCap = "round";
    ctx.shadowColor = "rgba(129,140,248,0.6)";
    ctx.shadowBlur = 8;

    ctx.beginPath();
    const n = this.points.length;
    if (n > 0) {
      for (let i = 0; i < n; i++) {
        const x = (i / Math.max(n - 1, 1)) * w;
        const y = h - (this.points[i] / this.max) * (h - 6) - 2;
        if (i === 0) ctx.moveTo(x, y);
        else ctx.lineTo(x, y);
      }
      ctx.stroke();

      const fill = ctx.createLinearGradient(0, 0, 0, h);
      fill.addColorStop(0, "rgba(129,140,248,0.25)");
      fill.addColorStop(1, "rgba(129,140,248,0)");
      ctx.lineTo(w, h);
      ctx.lineTo(0, h);
      ctx.closePath();
      ctx.fillStyle = fill;
      ctx.fill();
    }

    ctx.shadowBlur = 0;
    const label = formatBytes(this.max);
    ctx.fillStyle = "rgba(167,180,208,0.7)";
    ctx.font = "10px Consolas, monospace";
    ctx.fillText(label, w - ctx.measureText(label).width - 4, 12);
  }
}