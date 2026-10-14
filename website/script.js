(function () {
  const storageKey = "azra_waitlist_signups";
  const canvas = document.getElementById("space-scene");
  const ctx = canvas.getContext("2d", { alpha: false });
  const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  let particles = [];
  let width = 0;
  let height = 0;
  let frame = 0;

  function resizeScene() {
    const ratio = Math.min(window.devicePixelRatio || 1, 2);
    width = window.innerWidth;
    height = window.innerHeight;
    canvas.width = Math.floor(width * ratio);
    canvas.height = Math.floor(height * ratio);
    canvas.style.width = `${width}px`;
    canvas.style.height = `${height}px`;
    ctx.setTransform(ratio, 0, 0, ratio, 0, 0);

    const count = Math.min(120, Math.max(54, Math.floor((width * height) / 18000)));
    particles = Array.from({ length: count }, () => ({
      x: Math.random() * width,
      y: Math.random() * height,
      vx: (Math.random() - 0.5) * 0.18,
      vy: (Math.random() - 0.5) * 0.18,
      size: Math.random() * 1.6 + 0.35,
      tint: Math.random() > 0.62 ? "57, 242, 223" : "95, 125, 255"
    }));
  }

  function drawScene() {
    ctx.fillStyle = "#020303";
    ctx.fillRect(0, 0, width, height);

    const horizon = height * 0.58;
    ctx.strokeStyle = "rgba(57, 242, 223, 0.035)";
    ctx.lineWidth = 1;
    for (let i = 0; i < 7; i += 1) {
      const y = horizon + i * 48;
      ctx.beginPath();
      ctx.moveTo(width * 0.18, y);
      ctx.lineTo(width * 0.82, y - i * 8);
      ctx.stroke();
    }

    particles.forEach((particle, index) => {
      if (!reducedMotion) {
        particle.x += particle.vx;
        particle.y += particle.vy;
      }

      if (particle.x < -10) particle.x = width + 10;
      if (particle.x > width + 10) particle.x = -10;
      if (particle.y < -10) particle.y = height + 10;
      if (particle.y > height + 10) particle.y = -10;

      const pulse = 0.45 + Math.sin(frame * 0.018 + index) * 0.22;
      ctx.fillStyle = `rgba(${particle.tint}, ${0.35 + pulse * 0.3})`;
      ctx.beginPath();
      ctx.arc(particle.x, particle.y, particle.size, 0, Math.PI * 2);
      ctx.fill();
    });

    frame += 1;
    if (!reducedMotion) {
      requestAnimationFrame(drawScene);
    }
  }

  function isValidEmail(value) {
    return /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(value);
  }

  function saveLocalSignup(email) {
    const existing = JSON.parse(localStorage.getItem(storageKey) || "[]");
    const normalized = email.toLowerCase();
    if (!existing.includes(normalized)) {
      existing.push(normalized);
      localStorage.setItem(storageKey, JSON.stringify(existing));
    }
  }

  async function sendToEndpoint(endpoint, email, consent) {
    const response = await fetch(endpoint, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        email,
        consent,
        source: "azra-landing-page",
        submittedAt: new Date().toISOString()
      })
    });

    if (!response.ok) {
      throw new Error("Signup request failed.");
    }
  }

  function setStatus(form, message, isError) {
    const status = form.querySelector(".form-status");
    status.textContent = message;
    status.classList.toggle("error", Boolean(isError));
  }

  document.querySelectorAll(".signup").forEach((form) => {
    form.addEventListener("submit", async (event) => {
      event.preventDefault();

      const emailInput = form.querySelector('input[type="email"]');
      const consentInput = form.querySelector('input[name="consent"]');
      const email = emailInput.value.trim();
      const hasConsent = consentInput ? consentInput.checked : true;

      if (!isValidEmail(email)) {
        setStatus(form, "Enter a valid email address.", true);
        emailInput.focus();
        return;
      }

      if (!hasConsent) {
        setStatus(form, "Please confirm you want AZRA updates.", true);
        consentInput.focus();
        return;
      }

      const endpoint = form.dataset.endpoint && form.dataset.endpoint.trim();
      const button = form.querySelector("button");
      const label = form.dataset.submitLabel || button.textContent;
      button.disabled = true;
      button.textContent = "Joining...";

      try {
        if (endpoint) {
          await sendToEndpoint(endpoint, email, hasConsent);
        } else {
          saveLocalSignup(email);
        }

        setStatus(form, "You are on the AZRA early access list.");
        form.reset();
      } catch (error) {
        setStatus(form, "Could not join right now. Email hello@azrasoftware.com.", true);
      } finally {
        button.disabled = false;
        button.textContent = label;
      }
    });
  });

  resizeScene();
  drawScene();
  window.addEventListener("resize", resizeScene);
})();
