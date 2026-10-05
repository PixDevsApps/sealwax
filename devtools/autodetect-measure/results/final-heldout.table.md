| # | Domain | Group | Result (IMAP / SMTP, username plan) | Won by | Strict | E1 | E1+E2 | Final ms |
|---|---|---|---|---|---|---|---|---|
| 1 | runbox.com | nordic | mail.runbox.com:993 TLS<br>mail.runbox.com:465 TLS (local→addr) | autoconfig | yes | yes | yes | 60 |
| 2 | wemail.no | nordic | mail.wemail.no:993 TLS<br>smtp.wemail.no:465 TLS (addr→local) | autoconfig | yes | yes | yes | 127 |
| 3 | inbox.com | nordic | mail.inbox.com:993 TLS<br>smtp.inbox.com:465 TLS (addr→local) | autoconfig | yes | yes | yes | 126 |
| 4 | myopera.com | nordic | nothing found | - | yes | yes | yes | 1305 |
| 5 | servetheworld.net | nordic | nothing found | - | **no**: nothing found | **no**: nothing found | **no**: nothing found | 3150 |
| 6 | loopia.se | nordic | imap.loopia.se:993 TLS<br>mail.loopia.se:587 STARTTLS (addr→local) | own-host+probe | **no**: IMAP imap.loopia.se:993 Tls not accepted | **no**: IMAP imap.loopia.se:993 Tls not accepted | **no**: IMAP imap.loopia.se:993 Tls not accepted | 3550 |
| 7 | bahnhof.se | nordic | nothing found | - | **no**: nothing found | **no**: nothing found | **no**: nothing found | 3631 |
| 8 | bredband.net | nordic | nothing found | - | truth unknown | truth unknown | truth unknown | 280 |
| 9 | stofanet.dk | nordic | mail.stofanet.dk:993 TLS<br>smtp.stofanet.dk:465 TLS (addr→local) | probe | truth unknown | truth unknown | truth unknown | 444 |
| 10 | sol.dk | nordic | mail.sol.dk:993 TLS<br>smtp.sol.dk:465 TLS (addr→local) | autoconfig | yes | yes | yes | 130 |
| 11 | inet.fi | nordic | imap.inet.fi:993 TLS<br>smtp.inet.fi:465 TLS (addr→local) | own-host | **no**: IMAP imap.inet.fi:993 Tls not accepted | **no**: IMAP imap.inet.fi:993 Tls not accepted | yes | 421 |
| 12 | 1984.is | nordic | mail.1984.is:993 TLS<br>mail.1984.is:475 TLS (addr→local) | autoconfig | **no**: SMTP mail.1984.is:475 Tls not accepted | **no**: SMTP mail.1984.is:475 Tls not accepted | **no**: SMTP mail.1984.is:475 Tls not accepted | 295 |
| 13 | internet.is | nordic | imap.internet.is:993 TLS<br>mail.internet.is:465 TLS (addr→local) | own-host+probe | truth unknown | truth unknown | truth unknown | 1379 |
| 14 | free.fr | europe | imap.free.fr:993 TLS<br>smtp.free.fr:465 TLS (local→addr) | ispdb | **no**: username LocalPart wrong | yes | yes | 74 |
| 15 | sfr.fr | europe | imap.sfr.fr:993 TLS<br>smtp.sfr.fr:465 TLS (addr→local) | ispdb | yes | yes | yes | 228 |
| 16 | vodafonemail.de | europe | imap.vodafonemail.de:993 TLS<br>smtp.vodafonemail.de:465 TLS (addr→local) | autoconfig | yes | yes | yes | 164 |
| 17 | freenet.de | europe | mx.freenet.de:993 TLS<br>mx.freenet.de:587 STARTTLS (addr→local) | autoconfig | yes | yes | yes | 159 |
| 18 | virginmedia.com | europe | imap.virginmedia.com:993 TLS<br>smtp.virginmedia.com:465 TLS (addr→local) | autoconfig | yes | yes | yes | 312 |
| 19 | talktalk.net | europe | imap.talktalk.net:993 TLS<br>smtp.talktalk.net:465 TLS (addr→local) | own-host | **no**: IMAP imap.talktalk.net:993 Tls not accepted | **no**: IMAP imap.talktalk.net:993 Tls not accepted | **no**: SMTP smtp.talktalk.net:465 Tls not accepted | 629 |
| 20 | libero.it | europe | imapmail.libero.it:993 TLS<br>smtp.libero.it:465 TLS (addr→local) | autoconfig | yes | yes | yes | 259 |
| 21 | telefonica.net | europe | nothing found | own-host | **no**: nothing found | **no**: nothing found | **no**: nothing found | 2653 |
| 22 | ziggo.nl | europe | imap.ziggo.nl:993 TLS<br>smtp.ziggo.nl:587 STARTTLS (addr→local) | autoconfig | yes | yes | yes | 254 |
| 23 | telenet.be | europe | imap.telenet.be:993 TLS<br>smtp.telenet.be:465 TLS (addr→local) | srv | **no**: SMTP smtp.telenet.be:465 Tls not accepted | **no**: SMTP smtp.telenet.be:465 Tls not accepted | **no**: SMTP smtp.telenet.be:465 Tls not accepted | 248 |
| 24 | wp.pl | europe | imap.wp.pl:993 TLS<br>smtp.wp.pl:465 TLS (local→addr) | ispdb | yes | yes | yes | 239 |
| 25 | onet.pl | europe | imap.poczta.onet.pl:993 TLS<br>smtp.poczta.onet.pl:465 TLS (addr→local) | autoconfig | yes | yes | yes | 114 |
| 26 | seznam.cz | europe | imap.seznam.cz:993 TLS<br>smtp.seznam.cz:465 TLS (addr→local) | autoconfig | yes | yes | yes | 180 |
| 27 | sunrise.ch | europe | imap.sunrise.ch:993 TLS<br>mail.upcmail.net:587 STARTTLS (addr→local, confirm) | own-host+mx-autoconfig | **no**: SMTP mail.upcmail.net:587 StartTls not accepted | **no**: SMTP mail.upcmail.net:587 StartTls not accepted | **no**: SMTP mail.upcmail.net:587 StartTls not accepted | 2453 |
| 28 | sapo.pt | europe | imap.sapo.pt:993 TLS<br>smtp.sapo.pt:465 TLS (addr→local) | ispdb | yes | yes | yes | 234 |
| 29 | eircom.net | europe | nothing found | - | truth unknown | truth unknown | truth unknown | 4265 |
| 30 | uni-paderborn.de | custom | mail.uni-paderborn.de:993 TLS<br>mail.uni-paderborn.de:587 STARTTLS (addr→local) | srv | **no**: SMTP mail.uni-paderborn.de:587 StartTls not accepted | **no**: SMTP mail.uni-paderborn.de:587 StartTls not accepted | **no**: SMTP mail.uni-paderborn.de:587 StartTls not accepted | 4006 |
| 31 | uiowa.edu | custom | outlook.office365.com:993 TLS<br>smtp.office365.com:587 STARTTLS (addr, OAuth) | mx-preset | **no**: username Address wrong | **no**: username Address wrong | **no**: username Address wrong | 2774 |
| 32 | uio.no | custom | nothing found | probe | **no**: nothing found | **no**: nothing found | **no**: nothing found | 5114 |
| 33 | fanarkh.no | custom | imap.one.com:993 TLS<br>send.one.com:465 TLS (addr→local) | autoconfig | yes | yes | yes | 166 |
| 34 | andrews.edu | custom | imap.gmail.com:993 TLS<br>smtp.gmail.com:465 TLS (addr→local) | mx-preset | yes | yes | yes | 407 |
| 35 | unito.it | custom | imap.gmail.com:993 TLS<br>smtp.gmail.com:465 TLS (addr→local) | mx-preset | yes | yes | yes | 236 |
| 36 | port39.de | custom | port39.de:993 TLS<br>port39.de:465 TLS (addr→local) | well-known | **no**: IMAP port39.de:993 Tls not accepted | **no**: IMAP port39.de:993 Tls not accepted | **no**: IMAP port39.de:993 Tls not accepted | 4007 |
| 37 | gho-team.de | custom | imap.ionos.de:993 TLS<br>smtp.ionos.de:465 TLS (addr→local, confirm) | mx-probe | yes | yes | yes | 5318 |
| 38 | schule-kemmental.ch | custom | imap.hostpoint.ch:993 TLS<br>smtp.hostpoint.ch:465 TLS (addr→local, confirm) | mx-probe | **no**: IMAP imap.hostpoint.ch:993 Tls not accepted | **no**: IMAP imap.hostpoint.ch:993 Tls not accepted | **no**: IMAP imap.hostpoint.ch:993 Tls not accepted | 5110 |
| 39 | posta.istruzione.it | custom | no mail | mx-preset | yes | yes | yes | 232 |
| 40 | hoelderlin-heidelberg.de | custom | nothing found | - | **no**: nothing found | **no**: nothing found | **no**: nothing found | 818 |
| 41 | samfundet.no | custom | imap.samfundet.no:993 TLS<br>smtp.samfundet.no:465 TLS (addr→local) | own-host | **no**: username Address wrong | **no**: username Address wrong | **no**: username Address wrong | 287 |
| 42 | carlslid.se | custom | imap.broadview.se:993 TLS<br>smtp.broadview.se:465 TLS (addr→local, confirm) | mx-probe | truth unknown | truth unknown | truth unknown | 1711 |
