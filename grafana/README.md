# Grafana

`zebra-printers.json` ist ein wiederverwendbares Dashboard für alle
ZebraTamer-Instanzen. Es erwartet, dass `pi-init` die lokalen `zpl_*`-Metriken
über den Node Exporter auf Port `9100` bereitstellt. Dadurch tragen System- und
Druckermetriken dasselbe Target-Label `host`.

In Grafana unter **Dashboards → New → Import** die JSON-Datei hochladen und die
zentrale Prometheus-Datenquelle auswählen. Die Variablen **Pi** und **Drucker**
werden automatisch aus `zpl_printer_info` gefüllt. Neue Geräte benötigen daher
kein eigenes Dashboard.

Alte Drucker oder unidirektionale USB-Paralleladapter liefern nicht jeden
Statuswert. Das Dashboard zeigt solche Werte als „Nicht unterstützt“ und listet
die Ursache über `zpl_printer_observation_state`.
