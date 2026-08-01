<script lang="ts">
  let { size = 72, mood = "calm", animate = true } = $props<{
    size?: number;
    mood?: "calm" | "listening" | "thinking" | "happy" | "sleeping";
    animate?: boolean;
  }>();

  let imageAvailable = $state(true);
</script>

<div
  class:animate
  class:listening={mood === "listening"}
  class:thinking={mood === "thinking"}
  class:happy={mood === "happy"}
  class:sleeping={mood === "sleeping"}
  class="mascot"
  style:width={`${size}px`}
  style:height={`${size}px`}
  aria-label={`Woof is ${mood}`}
  role="img"
>
  {#if imageAvailable}
    <img
      src="/mascot/boxer-head.png"
      alt=""
      draggable="false"
      onerror={() => (imageAvailable = false)}
    />
  {:else}
    <svg viewBox="0 0 100 100" aria-hidden="true">
      <path class="ear left" d="M24 19 9 11 14 47 31 46Z" />
      <path class="ear right" d="m76 19 15-8-5 36-17-1Z" />
      <path class="head" d="M24 22C31 12 69 12 76 22c8 12 8 42-2 56-10 13-38 13-48 0-10-14-10-44-2-56Z" />
      <path class="blaze" d="M47 17c-6 14-7 27-5 40l8 22 8-22c2-13 1-26-5-40Z" />
      <path class="muzzle" d="M30 57c4-9 12-13 20-8 8-5 16-1 20 8 4 10-3 24-20 24S26 67 30 57Z" />
      <ellipse class="eye" cx="34" cy="43" rx="5" ry="6" />
      <ellipse class="eye" cx="66" cy="43" rx="5" ry="6" />
      <path class="brow left" d="m27 34 13-3" />
      <path class="brow right" d="m60 31 13 3" />
      <path class="nose" d="M42 58c2-4 14-4 16 0 1 4-3 8-8 8s-9-4-8-8Z" />
      <path class="mouth" d="M50 65v7m0 0c-5 0-8-2-10-4m10 4c5 0 8-2 10-4" />
      {#if mood === "sleeping"}
        <path class="lid left" d="m28 43 12 1" />
        <path class="lid right" d="m60 44 12-1" />
      {/if}
    </svg>
  {/if}
  <span class="glow"></span>
</div>

<style>
  .mascot {
    position: relative;
    display: grid;
    place-items: center;
    flex: 0 0 auto;
    transform-origin: 50% 74%;
    isolation: isolate;
  }

  img,
  svg {
    position: relative;
    z-index: 2;
    width: 100%;
    height: 100%;
    object-fit: contain;
    filter: drop-shadow(0 7px 9px rgba(60, 39, 29, 0.19));
  }

  .glow {
    position: absolute;
    z-index: 1;
    inset: 19% 12% 5%;
    border-radius: 50%;
    background: rgba(231, 173, 117, 0.3);
    filter: blur(12px);
    opacity: 0;
  }

  .head,
  .ear {
    fill: #cf9665;
  }

  .ear {
    fill: #4a3228;
  }

  .blaze,
  .muzzle {
    fill: #fff2d9;
  }

  .eye,
  .nose {
    fill: #2c201c;
  }

  .brow,
  .mouth,
  .lid {
    fill: none;
    stroke: #2c201c;
    stroke-width: 3.2;
    stroke-linecap: round;
  }

  .lid {
    stroke-width: 5;
  }

  .animate {
    animation: breathe 2.4s var(--ease) infinite;
  }

  .listening {
    animation: perk 0.82s var(--spring) infinite alternate;
  }

  .listening .glow,
  .thinking .glow {
    opacity: 1;
    animation: glow 1.25s ease-in-out infinite alternate;
  }

  .thinking {
    animation: tilt 1.8s var(--ease) infinite alternate;
  }

  .happy {
    animation: bounce 0.68s var(--spring) infinite alternate;
  }

  .sleeping {
    animation-duration: 3.6s;
    opacity: 0.8;
  }

  @keyframes breathe {
    0%,
    100% {
      transform: translateY(0) scale(1);
    }
    50% {
      transform: translateY(-1.5%) scale(1.015);
    }
  }

  @keyframes perk {
    from {
      transform: translateY(1px) scale(0.98);
    }
    to {
      transform: translateY(-3px) scale(1.03);
    }
  }

  @keyframes tilt {
    from {
      transform: rotate(-2deg);
    }
    to {
      transform: rotate(3deg);
    }
  }

  @keyframes bounce {
    from {
      transform: translateY(0) scale(1);
    }
    to {
      transform: translateY(-4px) scale(1.025);
    }
  }

  @keyframes glow {
    from {
      transform: scale(0.86);
      opacity: 0.38;
    }
    to {
      transform: scale(1.12);
      opacity: 0.82;
    }
  }
</style>
