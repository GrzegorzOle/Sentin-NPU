<!--
Copyright 2026 Grzegorz Oleksy
SPDX-License-Identifier: Apache-2.0
-->

# Wdrożenie Sentin-NPU w Wazuhu

Krok po kroku, dla administratora Wazuha. Nie trzeba czytać kodu w Ruście ani wiedzieć o bramie nic
poza tym, gdzie zapisuje swój ślad audytowy.

**Wersja angielska: [`deployment-en.md`](deployment-en.md).**

## Co ten przewodnik zakłada

| Założenie | Jeśli nie jest spełnione |
|---|---|
| Manager Wazuha działa i masz do niego dostęp po SSH z `sudo` | Najpierw go zainstaluj; nic tutaj nie zależy od sposobu instalacji |
| Maszyna z bramą jest już **zarejestrowanym agentem** i widnieje jako `Active` | Zarejestruj ją zwyczajnie (`agent-auth` albo polecenie wdrożeniowe z dashboardu); ten przewodnik zaczyna się po tym kroku |
| Wazuh **4.x**, testowane na 4.14 | Reguły nie używają niczego nowszego niż 4.0; wersji czuły jest tylko format obiektów zapisanych, stabilny od OpenSearch Dashboards 2.x |
| Brama jest zainstalowana i inspekcjonuje ruch | Zobacz `docs/install-windows.md` albo `docs/install-linux.md` w tym samym archiwum |

Wdrożenie zajmuje około dwudziestu minut, z czego jakieś piętnaście to import dashboardu.

## Gdzie leżą pliki

Dwa katalogi, i ten podział jest celowy: **to, co wdrażasz**, leży w `wazuh/`, a **to, co czytasz**,
tutaj, w `docs/wazuh/`.

| | Pliki do wdrożenia (`wazuh/`) | Dokumentacja (`docs/wazuh/`) |
|---|---|---|
| W paczce wydania | `wazuh/` | `docs/wazuh/` |
| Po instalacji na Windows | `C:\Program Files\Sentin-NPU\wazuh\` | `C:\Program Files\Sentin-NPU\docs\wazuh\` |
| Z AppImage | `--docs <katalog>` wypakowuje jedno i drugie do `<katalog>/wazuh/` | tamże |
| W repozytorium | `packaging/wazuh/` | `docs/wazuh/` |

Co jest w każdym z nich:

| Plik | Czym jest |
|---|---|
| `wazuh/sentin_npu_rules.xml` | 18 reguł, identyfikatory 100500-100531. Instalowane na **managerze**. |
| `wazuh/sentin-npu-dashboard.ndjson` | 15 paneli plus dashboard. Importowane w **Dashboards**. |
| `wazuh/deploy-manager.sh` | Wykonuje sekcje 3 i 4 za Ciebie, idempotentnie, z trybem próbnym. |
| `wazuh/build_dashboard.py` | Generuje ndjson na nowo. Potrzebny tylko przy przenumerowaniu reguł albo zmianie wzorca indeksu. |
| `docs/wazuh/examples/localfile-windows.conf` | Blok `<localfile>` dla pojedynczego agenta na Windows. |
| `docs/wazuh/examples/localfile-linux.conf` | To samo dla Linuksa. |
| `docs/wazuh/examples/agent-group.conf` | Forma `<agent_config>`, rozsyłana z managera do grupy. Obie platformy w jednym pliku. |
| `docs/wazuh/examples/gateway-audit.yaml` | Blok `audit:` z konfiguracji samej bramy. |
| `docs/wazuh/examples/sample-audit.jsonl` | Czternaście syntetycznych zdarzeń pokrywających każdą regułę. Do podania `wazuh-logtest`, zanim pojawi się prawdziwy ruch. |
| `docs/wazuh/examples/rotate-audit.ps1` | Rotacja na Windows. Brama nigdy nie rotuje własnego śladu. |
| `docs/wazuh/examples/logrotate-sentin-npu.conf` | To samo dla Linuksa. |

---

## 1. Wybór transportu

To jest decyzja o "protokole" i ma zalecaną odpowiedź.

| | **Plik JSONL + `localfile`** (zalecane) | CEF po syslogu | OTLP |
|---|---|---|---|
| Co robi brama | Dopisuje do pliku jeden obiekt JSON na linię | Wysyła linię CEF do nasłuchu syslog, UDP albo TCP | Wysyła OTLP po HTTP z kodowaniem JSON |
| Jak odbiera Wazuh | Kolektor agenta czyta plik, a wbudowany dekoder `json` wystawia każde pole jako `data.<nazwa>` | Nasłuch syslog na managerze, jeśli go włączysz | W ogóle |
| Czy dostarczone reguły zadziałają? | **Tak** | **Nie** - dopasowują nazwy pól JSON. Trzeba by napisać dekoder | Nie |
| Przetrwa niedostępność odbiorcy? | Tak, plik jest lokalny | Nie, UDP gubi po cichu | Nie |
| Wymaga otwarcia nowego nasłuchu | Nie | Tak | Tak |

Wybierz drogę przez plik, chyba że coś ją uniemożliwia. Tylko na niej żadne pole nie ginie na
formatowaniu, agent i tak już działa na tej maszynie, a **nie ma dekodera do napisania ani do
utrzymania** - brama pisze JSON, a parsowaniem zajmuje się własny dekoder JSON Wazuha.

Wariant z CEF opisuje sekcja 9, uczciwie razem z jego kosztem.

**Co musi być otwarte na normalnej ścieżce:** nic nowego. Agent już rozmawia z managerem po
**1514/TCP** (zdarzenia), a **1515/TCP** wykorzystał raz przy rejestracji. Ta integracja dokłada plik
na agencie i plik reguł na managerze - zero zmian sieciowych. Jeśli i tak chcesz sprawdzić:

```powershell
Test-NetConnection 192.168.88.4 -Port 1514        # Windows
```

```bash
ss -tnp | grep 1514                               # agent na Linuksie
```

## 2. Niech brama zacznie emitować

Blok `audit:` konfiguracji bramy, pełny przykład w
[`examples/gateway-audit.yaml`](examples/gateway-audit.yaml):

```yaml
audit:
  jsonl:
    enabled: true
    path: C:\ProgramData\Sentin-NPU\audit.jsonl     # zawsze bezwzględna
```

Konfiguracja leży w `C:\ProgramData\Sentin-NPU\config.yaml` na Windows, a na Linuksie w
`~/.config/sentin-npu/config.yaml` albo `/etc/sentin-npu/config.yaml`, zależnie od sposobu
instalacji. Na Windows konsola ustawień (`sentin-ui.exe`, w menu Start) zmienia te same wartości bez
otwierania YAML-a, sama prosi o podniesienie uprawnień i restartuje usługę.

**Używaj ścieżki bezwzględnej.** Względna rozwija się względem katalogu roboczego bramy - dla usługi
Windows jest to `C:\Windows\system32` - więc plik powstaje tam, gdzie nikt nie patrzy, a wszystkie
kontrole raportują sukces. To pułapka, w którą ten projekt wpadał częściej niż w jakąkolwiek inną.

Zrestartuj bramę i sprawdź, czy plik istnieje i rośnie:

```powershell
Restart-Service SentinNPU
Get-Item C:\ProgramData\Sentin-NPU\audit.jsonl | Select-Object Length, LastWriteTime
```

Czyste żądanie nie emituje **niczego** - celowo, bo SIEM pełen zdarzeń o niczym zasypuje te
prawdziwe. Wyślij więc przez bramę coś, co zostanie znalezione:

```powershell
curl.exe -s http://localhost:4141/openai/v1/chat/completions `
  -H "Content-Type: application/json" `
  -d '{\"model\":\"gpt-4o-mini\",\"messages\":[{\"role\":\"user\",\"content\":\"PESEL 02250514465\"}]}'
Get-Content C:\ProgramData\Sentin-NPU\audit.jsonl -Tail 3
```

`02250514465` to numer syntetyczny z poprawną cyfrą kontrolną - detektor sprawdza arytmetykę, więc
zmyślony numer, który się nie zgadza, niczego nie dowodzi. Zwróć uwagę, że sam identyfikator **nie**
pojawia się w powstałych zdarzeniach: tak właśnie działa schemat, to nie jest usterka.

## 3. Zbieranie po stronie agenta

**Wariant A, jedna maszyna.** Wklej blok z
[`examples/localfile-windows.conf`](examples/localfile-windows.conf) albo
[`examples/localfile-linux.conf`](examples/localfile-linux.conf) do pliku `ossec.conf` tego agenta,
wewnątrz `<ossec_config>`, i zrestartuj agenta:

```powershell
Restart-Service WazuhSvc                    # Windows, konsola administratora
```

```bash
sudo systemctl restart wazuh-agent          # Linux
```

**Wariant B, z managera, i to jego należy wybrać powyżej kilku maszyn.** Działa też tam, gdzie
`ossec.conf` w ogóle nie da się edytować, co na Windows oznacza każdą maszynę bez lokalnych
uprawnień administratora. Zainstaluj [`examples/agent-group.conf`](examples/agent-group.conf) jako
`agent.conf` grupy:

```bash
sudo /var/ossec/bin/agent_groups -a -g sentin-npu -q
sudo /var/ossec/bin/agent_groups -a -i <ID_AGENTA> -g sentin-npu -q

sudo cp agent-group.conf /var/ossec/etc/shared/sentin-npu/agent.conf
sudo chown wazuh:wazuh /var/ossec/etc/shared/sentin-npu/agent.conf
sudo chmod 660 /var/ossec/etc/shared/sentin-npu/agent.conf
sudo /var/ossec/bin/verify-agent-conf
sudo /var/ossec/bin/agent_control -R -u <ID_AGENTA>      # wypchnij teraz, zamiast czekać
```

Ostatnia linia nie jest przyspieszeniem, tylko krokiem procedury. Agent pobierze nową konfigurację
wspólną w ciągu minuty, po czym dalej będzie śledził **stary** plik aż do restartu, raportując zero
zdarzeń i zero strat - czyli dokładnie to, na co wygląda spokojny system.

**Nie zostawiaj kopii zapasowej w katalogu grupy.** Manager scala każdy plik z
`/var/ossec/etc/shared/<grupa>/` do `merged.mg`, więc `agent.conf.bak` nie jest kopią zapasową, tylko
drugą działającą konfiguracją - agent będzie zbierał obie ścieżki. Kopie trzymaj gdzie indziej. Ta
pułapka kosztowała już dzień pracy na działającym managerze.

Grupa jest warta dodatkowego kroku nawet dla jednego agenta: zmiana siedzi w jednym miejscu, które da
się przejrzeć, nie dotyka żadnego innego agenta, a usunięcie grupy usuwa integrację.

**Teraz sprawdź, czy agent naprawdę czyta plik**, zanim w ogóle zbliżysz się do managera:

```powershell
Get-Content "C:\Program Files (x86)\ossec-agent\wazuh-logcollector.state"     # Windows
```

```bash
grep -A3 audit.jsonl /var/ossec/var/run/wazuh-logcollector.state              # Linux
```

Plik musi się tam pojawić z rosnącym licznikiem `events` i zerem w `drops`. Na Windows ten plik stanu
da się odczytać bez podniesionych uprawnień, nawet gdy `ossec.conf` nie - to najszybszy sposób, żeby
odróżnić "nie zbierane" od "zbierane, ale żadna reguła nie dopasowała". Z poziomu dashboardu te dwa
stany wyglądają identycznie.

## 4. Instalacja reguł na managerze

Albo uruchom skrypt, który robi tę sekcję i grupę z sekcji 3, idempotentnie:

```bash
sudo ./deploy-manager.sh --dry-run        # wypisuje każdą zmianę, którą by wykonał
sudo ./deploy-manager.sh
```

Albo zrób to ręcznie:

```bash
sudo install -o wazuh -g wazuh -m 660 sentin_npu_rules.xml /var/ossec/etc/rules/
sudo /var/ossec/bin/wazuh-analysisd -t          # musi zakończyć się zerem PRZED restartem
sudo systemctl restart wazuh-manager
```

`wazuh-analysisd -t` parsuje cały zestaw reguł, nie ruszając działającej usługi. Na obciążonym
managerze to się liczy: błąd składni znaleziony tutaj kosztuje sekundę, a ten sam błąd znaleziony po
restarcie kosztuje tyle, ile zajmie komuś zauważenie, że alerty przestały przychodzić.

**Potem przetestuj na prawdziwych liniach - to ten krok wychwytuje rozjazd schematu.** Skopiuj
[`examples/sample-audit.jsonl`](examples/sample-audit.jsonl) na managera i podaj mu je:

```bash
sudo /var/ossec/bin/wazuh-logtest
# wklej jedną linię, a potem czytaj:
#   Phase 2: decoder 'json'
#   Phase 3: rule '100503' level 9
```

Oczekiwany werdykt dla kolejnych linii przykładowych:

| Linia | Zdarzenie | Reguła | Poziom |
|---|---|---|---|
| 1 | `gateway_start` | 100522 | 3 |
| 2, 3 | PESEL i IBAN `blocked` | 100501 | 12 |
| 4 | `decision_made` / `blocked` | 100510 | 10 |
| 5 | PESEL `masked` | 100503 | 9 |
| 6 | EMAIL `masked` | 100502 | 7 |
| 7 | PERSON `advised` | 100504 | 4 |
| 8 | LOCATION `observed` | 100505 | 3 |
| 9 | `decision_made` / `masked` | 100511 | 5 |
| 10 | IBAN `advised` wewnątrz PDF | 100507 | 10 |
| 11 | `attachment_skipped` | 100524 | 6 |
| 12 | `inspection_timeout` | 100520 | 8 |
| 13 | `device_fallback` | 100521 | 5 |
| 14 | `gateway_stop` | 100523 | 7 |

Trzech reguł częstotliwościowych (100525, 100530, 100531) nie da się tak przetestować: potrzebują
powtórzeń w oknie czasowym, a `wazuh-logtest` ocenia jedną linię naraz.

**Jeśli identyfikatory 100500-100531 są już zajęte na Twoim managerze**, przenumeruj plik i zmień
stałe z identyfikatorami reguł na górze `build_dashboard.py`, a potem wygeneruj ndjson na nowo. Pięć
paneli filtruje po identyfikatorze reguły i inaczej będzie pustych.

**Jeśli kiedykolwiek dodasz do bramy nowy rodzaj zdarzenia, dopisz go też do reguły 100500.** To
rodzic, na którym wiszą wszystkie pozostałe, a dziecko rodzica, który nigdy nie dopasowuje, nigdy nie
zadziała - po cichu. To nie jest teoria: `attachment_skipped` trafił do bramy i nie trafił do tej
linii, przez co reguła 100524 była martwa od dnia napisania, dopóki ktoś nie policzył alertów i nie
znalazł zera.

## 5. Import dashboardu

Dashboards -> **Stack Management** -> **Saved Objects** -> **Import** ->
`sentin-npu-dashboard.ndjson` -> *Automatically overwrite conflicts*. Powstaje szesnaście obiektów, z
przedrostkiem `sentin-npu-`.

Panele odwołują się do wzorca indeksu alertów przez identyfikator `wazuh-alerts-*`, czyli ten, którego
używa standardowa instalacja Wazuha. Jeśli Twój jest inny, wygeneruj pliki na nowo, zamiast poprawiać
piętnaście paneli ręcznie:

```bash
python3 build_dashboard.py --index-pattern 'identyfikator-twojego-wzorca'
```

Otwórz **Sentin-NPU - data leaving for LLMs**. Panele nie przypinają zakresu czasu, więc dashboard,
który nic nie pokazuje, to zwykle wybór zakresu, a nie zepsuty import - sprawdzaj to zawsze jako
pierwsze.

## 6. Weryfikacja end to end

Jedyna kontrola, która się liczy, to alert wywołany prawdziwym żądaniem. Z maszyny, na której działa
brama:

```powershell
curl.exe -s http://localhost:4141/openai/v1/chat/completions `
  -H "Content-Type: application/json" `
  -d '{\"model\":\"gpt-4o-mini\",\"messages\":[{\"role\":\"user\",\"content\":\"PESEL 02250514465, IBAN PL61109010140000071219812874\"}]}'
```

Potem na managerze:

```bash
sudo grep -c sentin /var/ossec/logs/alerts/alerts.json
sudo tail -n 40 /var/ossec/logs/alerts/alerts.json | grep -o '"id":"1005[0-9][0-9]"' | sort | uniq -c
```

Powinieneś zobaczyć 100501 albo 100503 dla identyfikatorów oraz 100510 albo 100511 dla żądania. Potem
znajdź te same alerty w dashboardzie - to potwierdza indekser i panele, a nie tylko managera.

Gdy czegoś brakuje, przechodź w tej kolejności, bo każdy krok wyklucza wszystko poniżej:

1. Czy `audit.jsonl` rośnie po wysłaniu żądania? Jeśli nie, problem jest w bramie, nie w Wazuhu.
2. Czy `wazuh-logcollector.state` pokazuje plik z rosnącym licznikiem? Jeśli nie, agent go nie czyta -
   ścieżka albo restart, który nigdy się nie odbył.
3. Czy `wazuh-logtest` dopasowuje linię z tego pliku? Jeśli nie, to reguły.
4. Czy alerty pojawiają się w `alerts.json`? Jeśli tak, a dashboard jest pusty, to zakres czasu albo
   wzorzec indeksu.

## 7. Co zobaczysz i co to znaczy

| Reguła | Poziom | Wyzwalana przez |
|---|---|---|
| 100500 | 0 | Kotwica. Nigdy nie alarmuje; jej dopasowanie dowodzi, że linia została zdekodowana jako nasz JSON |
| 100501 | 12 | Identyfikator **zablokowany** przed opuszczeniem maszyny |
| 100502 | 7 | Identyfikator **zamaskowany** przed opuszczeniem maszyny |
| 100503 | 9 | Jak wyżej, dla PESEL, IBAN i karty płatniczej - zgłaszalne same z siebie |
| 100504 | 4 | Wykrycie, o którym użytkownik został ostrzeżony i mógł je zignorować |
| 100505 | 3 | Wyłącznie obserwacja |
| 100506 | 6 | Ostrzeżenie dotyczące wykrycia **wewnątrz załącznika** |
| 100507 | 10 | Identyfikator wysokiej wartości wewnątrz załącznika |
| 100510 | 10 | Całe żądanie odrzucone przez politykę. Ktoś usłyszał "nie" i zapyta dlaczego |
| 100511 | 5 | Całe żądanie przekazane dalej z zamaskowanymi identyfikatorami |
| 100520 | 8 | Inspekcja się nie zakończyła; ten ruch mógł wyjść **bez sprawdzenia** |
| 100521 | 5 | Inferencja spadła na inne urządzenie |
| 100522 | 3 | Brama wystartowała |
| 100523 | 7 | Brama zatrzymana; inspekcji nie ma już na ścieżce |
| 100524 | 6 | Załącznika nie dało się odczytać, więc wyszedł bez sprawdzenia |
| 100525 | 10 | Sześć niesprawdzonych załączników w dziesięć minut |
| 100530 | 12 | Osiem zamaskowanych identyfikatorów w pięć minut - nawyk, nie przypadek |
| 100531 | 13 | Cztery zablokowane żądania w dziesięć minut - ktoś testuje politykę |

Waga alertu idzie za tym, na co operator może zareagować, a nie za tym, co brzmi dramatycznie.
Zablokowane żądanie jest najgłośniejsze, bo człowiek dostał odmowę. `advised` i `observed` to opinie
warstwy drugiej, które użytkownik mógł zignorować, a SOC wzywany do nich uczy się ignorować źródło.

**Pola, po których można odpytywać.** Wszystkie przychodzą pod `data.`, prosto z JSON-a;
[`../events.md`](../events.md) jest źródłem rozstrzygającym, a każda zmiana schematu aktualizuje ten
plik w tym samym commicie. Para mylona najczęściej to `data.model_id` (model NER, który **sprawdza**)
kontra `data.upstream_model` (model, do którego dane miały **trafić**). Pytanie "dokąd wychodzą nasze
dane" grupuje się po tym drugim.

**Zdarzenia nigdy nie zawierają wykrytego tekstu.** Żadne pole nie jest w stanie go pomieścić. Tam,
gdzie treść ma znaczenie dla korelacji, `content_sha256` obejmuje cały sprawdzany ładunek, a nie sam
identyfikator, więc nie da się go odtworzyć przez przeszukanie jedenastu cyfr. Właśnie ta własność
sprawia, że skierowanie tego śladu do SOC-a, który czyta wiele osób, daje się obronić.

**`client_addr` jest daną osobową** w większości wdrożeń, dokładnie tak jak każdy log proxy. Jest
zapisywany, bo decyzji bez właściciela nie da się obsłużyć. Jeśli Wasze zasady retencji tego
zabraniają, wyłącz cały sink, zamiast filtrować pole: ta sama wartość trafia do każdego emitera.

### Trzy rzeczy, które są dziś prawdą i Cię zaskoczą

Spisane, bo każda wygląda na zepsute wdrożenie, a nim nie jest:

- **`gateway_stop` nigdy nie jest emitowane.** Rodzaj zdarzenia istnieje, `docs/events.md` go
  wymienia, a reguła 100523 na niego czeka, ale żadna ścieżka w bramie go nie produkuje - więc ta
  reguła nie może zadziałać. Plik przykładowy zawiera tę linię, żeby dało się regułę przetestować;
  nie traktuj braku 100523 jako dowodu, że brama nadal działa.
- **Opis reguły 100521 nie nazywa urządzenia.** `device_fallback` niesie parę jako
  `data.detail.requested` i `data.detail.actual`, a nie w polu `device` najwyższego poziomu, które
  podstawia opis - więc tekst kończy się na "now". Informacja jest w alercie, tylko poziom niżej.
- **Opis reguły 100524 nie nazywa modelu.** `attachment_skipped` nie niesie `upstream_model`, więc po
  słowie "towards" nie ma nic.

### Ograniczenie dwóch reguł powtórzeniowych

100530 i 100531 liczą **per agent**, a nie per adres źródłowy. Wazuh 4.14 przestaje wyzwalać regułę
częstotliwościową w ogóle, gdy tylko doda się jakiekolwiek ograniczenie `same_*` - sprawdzone przez
`same_field` na `client_addr` i na `data.client_addr` oraz przez `same_location`, wobec reguły
kontrolnej, która bez nich działa niezawodnie. Reguła, która po cichu nigdy nie zadziała, jest gorsza
od zgrubnej, która działa.

W praktyce to prawie to samo, bo brama działa na jednej stacji, której agent raportuje jedną maszynę.
Na bramie **współdzielonej**, obsługującej wielu dzwoniących, to już nie to samo: czytaj adres z
alertu, zamiast ufać grupowaniu, i pamiętaj, że opis podaje adres tego zdarzenia, które przekroczyło
próg, a nie jedynego, które się na próg złożyło.

## 8. Diagnostyka

**Nic nie ma w dashboardzie.** W tej kolejności, bo pierwsza odpowiedź jest trafna częściej niż cała
reszta razem wzięta:

1. Zakres czasu.
2. Czy agent czyta plik? `wazuh-logcollector.state`, jak w sekcji 3.
3. Czy alerty w ogóle powstają? `grep sentin /var/ossec/logs/alerts/alerts.json`.
4. Czy zdarzenie dochodzi, ale nic nie dopasowuje? **Log, którego nie łapie żadna reguła, jest
   porzucany po cichu** - bez alertu i bez archiwum, o ile nie włączono `logall_json`. Pusty dashboard
   przy zdrowym kolektorze oznacza dokładnie to. `wazuh-logtest` odpowiada w kilka sekund.
5. Identyfikator wzorca indeksu w imporcie. Panel ze zepsutym odwołaniem renderuje się pusty, zamiast
   zgłosić błąd.

**Alerty przychodzą na poziomie 3 z ogólnym opisem.** Coś innego dopasowało się pierwsze albo reguła
100500 nie dopasowała się wcale. Sprawdź, czy zadziałał dekoder JSON - `Phase 2: decoder 'json'` w
`wazuh-logtest`. Jeśli linia została zebrana jako `syslog`, a nie `json`, pól po prostu nie ma do
dopasowania, a `log_format` w bloku `localfile` jest błędny.

**Reguły nie działają po edycji.** Manager trzeba zrestartować; `wazuh-analysisd -t` tylko waliduje.
A `<if_sid>` rozwiązuje się w kolejności pliku, więc dziecko zdefiniowane przed rodzicem jest martwe.

**Wszystko działało i przestało po rotacji.** Kolektor podąża za obcięciem pliku w miejscu. Jeśli
Wasza rotacja zamiast tego przenosi plik i tworzy nowy, zdarzenia zapisane pomiędzy jednym a drugim
przepadają. Używaj `copytruncate` - patrz
[`examples/logrotate-sentin-npu.conf`](examples/logrotate-sentin-npu.conf) i
[`examples/rotate-audit.ps1`](examples/rotate-audit.ps1).

**Zdarzenia ustały po aktualizacji bramy.** Sprawdź `audit.jsonl.path` w tej konfiguracji, którą
zaktualizowana brama faktycznie czyta. Instalator, który zapisuje `config.yaml.new` obok istniejącego
`config.yaml`, celowo nie zmienił Twoich ustawień - ale jeśli ktoś przyjął nowy plik, ścieżka mogła
się przenieść.

**Zdarzenia nie mają pola `device` ani `model_id`.** To nie jest problem Wazuha: brama działa z
niedostępną warstwą 2, więc pracują wyłącznie detektory sumy kontrolnej. Poszukaj `layer 2 ready` w
`C:\ProgramData\Sentin-NPU\sentin-gateway.log`. Warto zrobić na to własny alert: brama sprawdzająca
połowę tego, co deklaruje, z każdej innej strony wygląda zdrowo.

## 9. Wariant z CEF

Jeśli czytanie pliku jest niemożliwe, brama emituje też CEF po syslogu:

```yaml
audit:
  syslog_cef:
    enabled: true
    address: 192.168.88.4:514
    protocol: udp
```

Pola lądują jako rozszerzenia CEF: `src` i `spt` dla dzwoniącego, `cs5` model docelowy, `cs6`
dostawca, `cs1` detektor, `cs2` typ danych, `cs3` identyfikator modelu, `cs4` urządzenie, `act`
decyzja, `dhost` host docelowy, `fileHash` skrót treści, a pola załącznika mapują się na `cat`,
`fileType` i `fsize`.

**Reguły z tej integracji dopasowują nazwy pól JSON i na CEF nie zadziałają.** Trzeba by napisać
własny dekoder. To jest uczciwy powód, dla którego droga przez plik jest zalecana, a nie tylko
preferowana.

OTLP też jest dostępny i wykracza poza ten dokument: Wazuh nie ma odbiornika OTLP.

## 10. Usuwanie

```bash
sudo rm /var/ossec/etc/rules/sentin_npu_rules.xml
sudo /var/ossec/bin/agent_groups -r -i <ID_AGENTA> -g sentin-npu -q
sudo systemctl restart wazuh-manager
```

Potem usuń obiekty zapisane w Dashboards - wszystkie mają przedrostek `sentin-npu-` - i skasuj blok
`<localfile>` z agenta, jeśli dodawałeś go ręcznie w wariancie A.

Wyłączenie sinka JSONL w bramie to osobna decyzja: ten sam plik jest tym, z czego konsola ustawień
buduje raport HTML offline, na maszynach, które nie mają żadnego SIEM-u.
