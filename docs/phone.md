# Using the app from a phone

The Mac keeps doing the work. The phone shows every task and its agents,
follows an agent's terminal live, and, if you allow it, answers one: a line
of text, or the keys a question takes. What it may and may not do is in
`features.md`, §17.

There are two ways for the phone to reach the Mac. Use either, or both.

| | Tailscale | Home network and router VPN |
|---|---|---|
| Works from | anywhere | the same Wi-Fi; anywhere through the router's VPN |
| Needs | Tailscale on the Mac and the phone | a router that runs a VPN server (a GL.iNet, say), and a public IP address at home |
| Switch in Settings → Phone | Tailscale | Home network and router VPN |

## The same on both: pair the phone

1. In the app, **Settings → Phone**, switch on the way you set up below.
   "Open on the phone" then lists this Mac's address on it, like
   `http://100.101.102.103:7420`.
2. Open that address in the phone's browser.
3. In the app, choose **Pair a phone**. Type the six-digit code it shows
   on the phone. It works once, for two minutes.
4. On an iPhone: Share → **Add to Home Screen**, so it opens like an app.
5. To let the phone answer agents, switch on **Let phones type into
   agents**. Off, it only watches.

A phone you no longer use: **Forget** in the list. It is out at once.

**Keep the Mac awake.** Turn on the cup in the top bar. With a way in on,
it holds the Mac awake while any agent is running, not only while one is
working, so one waiting on you is still there to answer. Closing the lid
still sleeps the Mac; leave it open, or use it closed only on power with an
external display.

## Tailscale

The general way: no router settings, works behind any network.

1. Install Tailscale on the Mac (tailscale.com/download, or the Mac App
   Store) and sign in.
2. Install Tailscale on the phone and sign in to the same account.
3. In the app, switch on **Tailscale**, and pair as above using the
   `100.x.y.z` address.

The phone needs Tailscale connected whenever you use the app from it.
Tailscale's MagicDNS name for the Mac (`http://your-mac:7420`) works too.

## Home network and a GL.iNet router's VPN

On the same Wi-Fi as the Mac this needs nothing else: switch on **Home
network and router VPN** and use the `192.168…` address. To reach it from
outside, the router lets the phone into the home network over WireGuard.
The menus below are a GL.iNet's (firmware 4); other routers have the same
pieces under other names.

1. **Check the router has a public address.** The WAN IP on the router's
   home page must be the one a site like whatismyip.com shows. If they
   differ, your provider shares one address among many customers (CGNAT)
   and nothing outside can reach the router; use Tailscale instead, on the
   Mac or on the router (Applications → Tailscale, as a subnet router).
2. **Dynamic DNS:** Applications → Dynamic DNS, on. The router gets a name
   that follows your home address.
3. **WireGuard server:** VPN → WireGuard Server. Start it, add a profile
   for the phone, and allow access to the local network.
4. **On the phone:** install the WireGuard app and scan the profile's QR
   code. To send only home traffic through it, set the profile's Allowed
   IPs to your home network (`192.168.8.0/24` on a GL.iNet by default). The
   iOS app can connect on its own whenever you leave your Wi-Fi ("On
   Demand").
5. **Give the Mac a fixed address:** in the router's client list, reserve
   the Mac's current address, so the one saved on the phone stays right.
6. In the app, switch on **Home network and router VPN**, and pair as
   above using the `192.168…` address.

## What to know

- **Plain HTTP.** Tailscale and WireGuard encrypt the way from the phone.
  On the home Wi-Fi itself the traffic is not encrypted, so anything else on
  that network could read it. Put guests on the router's guest network.
- **macOS may ask** whether Villain Layer may accept incoming connections
  the first time a way is on. Allow it. The dev build asks for itself.
- **The port** is 7420 (the dev build's 7421). If something else holds
  it, Settings → Phone says so.
- **Only agents take typing**, never shells, and only a line at a time:
  at most 512 bytes, as a terminal keeps.
