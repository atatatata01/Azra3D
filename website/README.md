# AZRA Landing Page

Open `index.html` in a browser to view the static landing page.

The mailing-list forms validate email addresses and save signups to local browser storage by default for demo mode. To connect a real provider, set `data-endpoint` on each `.signup` form in `index.html` to a POST endpoint that accepts JSON:

```json
{
  "email": "artist@studio.com",
  "consent": true,
  "source": "azra-landing-page",
  "submittedAt": "2026-07-02T00:00:00.000Z"
}
```

Good provider options for this kind of early-access list are Buttondown, Beehiiv, ConvertKit, Mailchimp, or Formspree.
