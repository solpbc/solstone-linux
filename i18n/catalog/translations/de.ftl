linux.about.ageday = { $count ->
    [one] vor { $formatted } Tag
   *[other] vor { $formatted } Tagen
}

linux.about.agehour = { $count ->
    [one] vor { $formatted } Stunde
   *[other] vor { $formatted } Stunden
}

linux.about.ageminute = { $count ->
    [one] vor { $formatted } Minute
   *[other] vor { $formatted } Minuten
}

linux.about.app = Linux-Desktop-App

linux.about.block = { $app }{ $break }{ $journal }

linux.about.journal = journal

linux.about.lastseen = { $journal } · zuletzt gesehen { $age }

linux.about.now = gerade eben

linux.about.unknown = Journal unbekannt

linux.about.version = { $name } { $version }

linux.about.versionarch = { $name } { $version } · { $arch }

linux.about.versionbuild = { $name } { $version } ({ $build })

linux.about.versionbuildarch = { $name } { $version } ({ $build }) · { $arch }

linux.about.versionbuildos = { $name } { $version } ({ $build }) · { $os }

linux.about.versionbuildosarch = { $name } { $version } ({ $build }) · { $os } · { $arch }

linux.about.versionbuildosversion = { $name } { $version } ({ $build }) · { $os } { $osversion }

linux.about.versionbuildosversionarch = { $name } { $version } ({ $build }) · { $os } { $osversion } · { $arch }

linux.about.versionos = { $name } { $version } · { $os }

linux.about.versionosarch = { $name } { $version } · { $os } · { $arch }

linux.about.versionosversion = { $name } { $version } · { $os } { $osversion }

linux.about.versionosversionarch = { $name } { $version } · { $os } { $osversion } · { $arch }

linux.bin.solstoneterminal.couldnotinstallsiginthandler = konnte den { $literal1 }-Handler nicht installieren: { $err }

linux.bin.solstoneterminal.couldnotinstallsigtermhandler = konnte den { $literal1 }-Handler nicht installieren: { $err }

linux.bin.solstoneterminal.executablemustbeabsolute = { $value1 }: die Programmdatei muss absolut sein

linux.bin.solstoneterminal.nothinghere = { $value1 }: hier ist nichts

linux.bin.solstoneterminal.stillrunning = { $value1 }: läuft noch

linux.browser.status.browserconnected = Browser: { $value1 } verbunden

linux.browser.status.browserpagesthiscomputerwaiting = { $ws1 }{ $value1 } Browserseiten auf diesem Computer warten darauf, in dein Journal zu gehen

linux.browser.status.chromeedge = Chrome oder Edge

linux.browser.status.ifyouhaventyet = { $ws1 }falls noch nicht, füge die solstone-Erweiterung zu { $served } hinzu: { $url }

linux.browser.status.n0kb = { $value1 } KB

linux.browser.status.n1mb = { $value1 } MB

linux.cli.agreewithoutbeingasked = zustimmen, ohne gefragt zu werden

linux.cli.anothercopyofthesolstoneappisalreadyrunning = Eine weitere Kopie der solstone-App läuft schon für diese Anmeldung, deshalb ist diese nicht gestartet. Beende die andere Kopie und versuch es noch einmal.

linux.cli.browserendpointisnottrusted = Browser-endpoint ist nicht vertrauenswürdig

linux.cli.cachex = Zwischenspeicher:{ $ws1 }{ $value1 }

linux.cli.cachexnotcreatedyet = Zwischenspeicher:{ $ws1 }{ $value1 } (noch nicht angelegt)

linux.cli.commandoutputexceededitsbound = die Befehlsausgabe hat ihre Grenze überschritten

linux.cli.commandtimedout = Zeitüberschreitung beim Befehl

linux.cli.configupdateerrorx = Fehler bei der Konfigurationsaktualisierung: { $error }

linux.cli.configx = Konfiguration: { $value1 }

linux.cli.confirmyourjournalsmarkexitcodes0pairedand = bestätige die mark deines Journals.{ $ws1 }Exit-Codes:{ $ws2 }0{ $ws3 }gekoppelt und bestätigt, oder schon bestätigt{ $ws4 }1{ $ws5 }nicht gekoppelt, nichts geändert, oder kein Terminal{ $ws6 }2{ $ws7 }Nutzungsfehler{ $ws8 }5{ $ws9 }zurückgehalten

linux.cli.couldnotdiscardthebrowserpagesx = konnte die Browserseiten nicht verwerfen: { $error }

linux.cli.couldnotstartx = konnte nicht starten: { $error }

linux.cli.couldntkeepthatlinkforthe = konnte diesen Link nicht für das Fenster behalten: { $error }

linux.cli.credentialabsent = Zugangsdaten fehlen

linux.cli.discardfailed = verwerfen ist nicht gelungen

linux.cli.discardreplytoolarge = Antwort aufs Verwerfen ist zu groß

linux.cli.discardwaitingbrowserpages = wartende Browserseiten verwerfen

linux.cli.drawcursor = Cursor zeichnen

linux.cli.editsettings = Einstellungen bearbeiten

linux.cli.enterapositiveinteger = Gib eine positive ganze Zahl ein.

linux.cli.enteryorn = Gib { $literal1 } oder { $literal2 } ein.

linux.cli.erroreditingsettingsx = Fehler beim Bearbeiten der Einstellungen: { $error }

linux.cli.errorx = Fehler: { $error }

linux.cli.howmanyminutestopausefordefaultuntilyouresu = Wie viele Minuten pausiert werden (Standard: bis du fortsetzt)

linux.cli.installgnomeandkwinactivityhelperassets = { $command1 }- und { $command2 }-Helfer für Aktivität installieren

linux.cli.installsystemduserservice = systemd-Benutzerdienst installieren

linux.cli.invaliddiscardreply = ungültige Antwort aufs Verwerfen

linux.cli.journalversionx = journal-Version: { $version-str }

linux.cli.linkedprivatestateisalreadyinuse = Verknüpfter privater Zustand ist schon in Verwendung

linux.cli.missingexecutabledirectory = Verzeichnis der Programmdatei fehlt

linux.cli.moveexistingsolstonetmuxinto = ein vorhandenes solstone für { $command0 } in diese App verschieben

linux.cli.nosessionbussothereisnodesktoptosetupx = kein Session-Bus, deshalb kein Desktop zum Einrichten: { $error }

linux.cli.notset = (nicht gesetzt)

linux.cli.pairthisdevicewithyourjournal = dieses Gerät mit deinem Journal koppeln

linux.cli.pairthisdevicewithyourjournalexitcodes0pair = dieses Gerät mit deinem Journal koppeln.{ $ws1 }Exit-Codes:{ $ws2 }0{ $ws3 }gekoppelt und bestätigt, oder schon bestätigt{ $ws4 }1{ $ws5 }nicht gekoppelt, nichts geändert, oder kein Terminal{ $ws6 }2{ $ws7 }Nutzungsfehler{ $ws8 }5{ $ws9 }zurückgehalten

linux.cli.pauseintake = Annahme pausieren

linux.cli.resumeintake = Annahme fortsetzen

linux.cli.screenaudiodesktopactivityoff = Bildschirm, Audio und Desktop-Aktivität: an oder aus

linux.cli.segmentdurationinsecondsdefault300 = Segmentdauer in Sekunden (Standard: 300)

linux.cli.segmentintervalseconds = Segmentintervall in Sekunden

linux.cli.servicex = { $ws1 }Dienst: { $value1 }

linux.cli.settingsboolprompt = { $label } [{ $answer }]:{ $ws1 }

linux.cli.settingsintprompt = { $label } [{ $current }]:{ $ws1 }

linux.cli.settingssavedtox = { $ws1 }Einstellungen gespeichert unter { $value1 }

linux.cli.setupappalreadyrunning = die Ersteinrichtung konnte nicht starten, weil die solstone-App läuft. beende die solstone-App zuerst und versuch es noch einmal. es wurde keine Eingabe verarbeitet. App-Zustand, Konfiguration und privater Zustand sind unverändert.

linux.cli.setupconfigfailed = die Ersteinrichtung ist vor der Kopplung fehlgeschlagen, weil die solstone-App ihre Konfiguration nicht sicher aktualisieren konnte.

linux.cli.setupconnectionfailed = Ersteinrichtung fehlgeschlagen: die solstone-App konnte sich nicht mit deinem Journal verbinden.

linux.cli.setupfailedx = Ersteinrichtung fehlgeschlagen: { $error }

linux.cli.setupthegnomepaneliconwherepauseandresumel = das { $command1 }-Panel-Symbol einrichten, wo Pausieren und Fortsetzen liegen

linux.cli.showstatus = Status anzeigen

linux.cli.showwhetherterminaliswhat = zeige, ob das Terminal an ist und was es tut

linux.cli.showyourappandjournalversions = zeige die Versionen deiner App und des Journals

linux.cli.startpaused = Pausiert starten

linux.cli.startupcouldnotcontinuebecausethesolstoneapp = Der Start konnte nicht weitergehen, weil die solstone-App den Status des Abgleichs vom vorherigen Lauf nicht löschen konnte. Sieh nach, dass die solstone-App ihre lokalen Daten schreiben kann, und versuch es noch einmal.

linux.cli.startupcouldnotcontinuebecausethesolstoneapp2 = Der Start konnte nicht weitergehen, weil die solstone-App ihre lokalen Daten nicht sicher vorbereiten konnte. Sieh nach, dass die solstone-App ihre lokalen Daten schreiben kann, und versuch es noch einmal.

linux.cli.streamnamedefaultstohostnamederived = Name des Datenstroms (Standard: vom Hostnamen abgeleitet)

linux.cli.streamx = Datenstrom: { $stream }

linux.cli.terminaloffstatus = Terminal: { $command3 } oder Status

linux.cli.thesetakeeffectthenexttimethesolstoneappsta = Das gilt beim nächsten Start der solstone-App.

linux.cli.thesolstoneappforlinuxtakesinwhatyousharew = die solstone-App für Linux nimmt entgegen, was du mit ihr teilst, und alles davon geht in dein Journal. Teil von solstone.

linux.cli.turnterminal = Terminal einschalten

linux.cli.turnterminaloff = Terminal ausschalten

linux.cli.uninstallsystemduserservice = systemd-Benutzerdienst deinstallieren

linux.cli.verifyinstallprerequisites = Installationsvoraussetzungen überprüfen

linux.cli.windowscopeexitedx = Fenster-Scope beendet: { $status }

linux.cli.xincompletesegments = { $ws1 }{ $value1 } unvollständige Segmente

linux.cli.xsegmentsacrossxdaysxmb = { $ws1 }{ $value1 } Segmente über { $value2 } Tag(e), { $value3 } MB

linux.clipboard.agentinstructions = solstone-App für Linux (Repository: { $repo })
    Quelle: { $repository }
    Lies INSTALL.md unter { $install } für Einrichtung und Aufbau.
    Konfiguration: { $config_path }
    Segmente: { $captures_dir }
    Protokolle: { $logs }
    Dienst: { $service }

linux.control.screenandaudioneedyourdesktopsession = Bildschirm und Audio brauchen deine Desktop-Sitzung, und keine läuft

linux.control.solstonefortmuxhadbeenstoppedso = { $ws1 }solstone für { $literal2 } war beendet, deshalb bleibt das Terminal aus

linux.control.terminalhistorycontinued = { $ws1 }Terminalverlauf: fortgesetzt

linux.control.terminalhistorycontinuedundertheoldpairing = { $ws1 }Terminalverlauf: fortgesetzt, unter der alten Kopplung

linux.control.thesolstoneappdidnotanswerin = die solstone-App hat nicht rechtzeitig geantwortet. versuch es in einem Moment noch einmal.

linux.control.thesolstoneappisntrunning = { $ws1 }die solstone-App läuft nicht

linux.control.thesolstoneappisntrunningstart = die solstone-App läuft nicht. starte sie und versuch es noch einmal.

linux.control.thesolstoneappisstartingtryagain = die solstone-App startet. versuch es in einem Moment noch einmal

linux.control.thisneedsattentionpleasecontactsupport = { $ws1 }das braucht Aufmerksamkeit: bitte wende dich an den Support

linux.copy.aboutheading = über solstone

linux.copy.aboutmadeby = von { $literal1 }.

linux.copy.activityonly = nur Bildschirmaktivität

linux.copy.activityroute = Route der Bildschirmaktivität

linux.copy.anythingwaitingsyncstaysthis = alles, was auf den Abgleich wartet, bleibt auf diesem Computer. um zu sehen, was zu tun ist, führe aus: { $literal1 }

linux.copy.anythingwaitingtosyncstaysonthiscomputerthe = alles, was auf den Abgleich wartet, bleibt auf diesem Computer. die solstone-App versucht es noch einmal, wenn dein Journal erreichbar ist.

linux.copy.appsleftoutofdesktopactivity = Apps, die bei der Desktop-Aktivität ausgelassen sind

linux.copy.appversion = App-Version

linux.copy.attentioncount = { $count ->
    [one] { $formatted } braucht Aufmerksamkeit
   *[other] { $formatted } brauchen Aufmerksamkeit
}

linux.copy.audio = Audio

linux.copy.audiointakecancontinue = die Audio-Annahme kann weitergehen.

linux.copy.availablelogsareshownbelowcheckthenotesforg = verfügbare Protokolle stehen unten. prüfe die Notizen auf Lücken, bevor du eine Kopie speicherst.

linux.copy.cancelpairing = Kopplung abbrechen

linux.copy.checkingstatus = Status wird geprüft…

linux.copy.checkstatus = Status prüfen

linux.copy.choose = wählen

linux.copy.choosefolder = Ordner wählen

linux.copy.choosefolder.v4cba918b = Ordner wählen…

linux.copy.chooseobsidianvault = wähle deinen { $literal1 }-Tresor

linux.copy.configfile = Konfigurationsdatei

linux.copy.confirming = wird bestätigt…

linux.copy.confirmyourjournalsmark = die mark deines Journals bestätigen

linux.copy.connected = verbunden mit

linux.copy.connecting = wird verbunden…

linux.copy.connectjournal = mit deinem Journal verbinden

linux.copy.copysaved = Kopie gespeichert.

linux.copy.couldntcheckwhetherallyoursettingsweresaved = ob alle deine Einstellungen gespeichert wurden, war nicht zu prüfen. lies sie erneut, bevor du es noch einmal versuchst.

linux.copy.couldntcheckwhetherintakepausedcheckstatusbe = ob die Annahme pausiert hat, war nicht zu prüfen. prüfe den Status, bevor du es noch einmal versuchst.

linux.copy.couldntcheckwhetherintakeresumedcheckstatusb = ob die Annahme fortgesetzt wurde, war nicht zu prüfen. prüfe den Status, bevor du es noch einmal versuchst.

linux.copy.couldntcheckwhetherpairingfinishedcheckstatus = ob die Kopplung fertig wurde, war nicht zu prüfen. prüfe den Status, bevor du es noch einmal versuchst.

linux.copy.couldntopenthatlinkvisitsupportsolstoneappo = dieser Link konnte nicht geöffnet werden. besuche { $literal1 } oder schreib an { $literal2 }.

linux.copy.couldntopenthesolstonewindow = das solstone-Fenster konnte nicht geöffnet werden

linux.copy.couldntopenthesolstonewindowopenxfordetails = das solstone-Fenster konnte nicht geöffnet werden. öffne { $path } für Details.

linux.copy.couldntopenyourbrowsertryagainoropenyourjo = dein Browser konnte nicht geöffnet werden. versuch es noch einmal, oder öffne dein Journal über seine übliche Adresse.

linux.copy.couldntpauseintaketryagain = die Annahme konnte nicht pausiert werden. versuch es noch einmal.

linux.copy.couldntreadthelogfilestryagain = die Protokolldateien konnten nicht gelesen werden. versuch es noch einmal.

linux.copy.couldntreadthelogstryagain = die Protokolle konnten nicht gelesen werden. versuch es noch einmal.

linux.copy.couldntreadthesolstoneappsstatuscheckwhethe = der Status der solstone-App konnte nicht gelesen werden. prüfe, ob sie läuft, und versuch es noch einmal.

linux.copy.couldntreadyoursettingstryagainbeforemaking = deine Einstellungen konnten nicht gelesen werden. versuch es noch einmal, bevor du etwas änderst.

linux.copy.couldntrestartthesolstoneappcheckthedetails = die solstone-App konnte nicht neu gestartet werden. prüfe die Details und versuch es noch einmal.

linux.copy.couldntresumeintaketryagain = die Annahme konnte nicht fortgesetzt werden. versuch es noch einmal.

linux.copy.couldntsaveactivityintaketryagain = die Annahme der Bildschirmaktivität konnte nicht gespeichert werden. versuch es noch einmal.

linux.copy.couldntsaveallyoursettingsyoureditsarestill = nicht alle deine Einstellungen konnten gespeichert werden. deine Änderungen sind noch hier. versuch es noch einmal.

linux.copy.couldntsavethecopytrysavingtoanotherfolder = die Kopie konnte nicht gespeichert werden. versuch, sie in einem anderen Ordner zu speichern.

linux.copy.couldntsaveyouranswertryagain = deine Antwort konnte nicht gespeichert werden. versuch es noch einmal.

linux.copy.couldntsaveyoursettingscheckthedetailsthent = deine Einstellungen konnten nicht gespeichert werden. prüfe die Details und versuch es noch einmal.

linux.copy.couldntstartthesolstoneappcheckthedetailsth = die solstone-App konnte nicht gestartet werden. prüfe die Details und versuch es noch einmal.

linux.copy.couldntverify = konnte nicht überprüfen

linux.copy.couldntverifyjournalso = dein Journal konnte nicht überprüft werden, deshalb ist nichts verbunden. versuch es noch einmal, oder füge einen Kopplungslink ein.

linux.copy.couldntworkoutthismarksotheresnothingtoco = diese mark war nicht zu erkennen, deshalb gibt es nichts zum Vergleichen. deine aktuelle Kopplung bleibt in Verwendung.

linux.copy.couldntworkoutyourjournalsmarksotheresnot = die mark dieses Journals war nicht zu erkennen, deshalb gibt es nichts zum Vergleichen. hol einen neuen Kopplungslink aus deinem Journal und versuch es noch einmal.

linux.copy.countwaitingsync = warten auf den Abgleich: { $count }

linux.copy.d1 = solange das an ist, nimmt die solstone-App die Bildschirmfotos entgegen, die du auf diesem Computer machst, und dein Journal behält jedes mit der Minute, in der du es gemacht hast.

linux.copy.d10 = manche Werkzeuge speichern ein Bildschirmfoto nur, wenn du sie dazu aufforderst, und ein Bildschirmfoto, das nicht gespeichert ist, kommt nicht herein.

linux.copy.d15 = mehr dazu

linux.copy.d8 = konnte auf diesem Computer keinen Ordner Screenshots finden. wähle den Ordner, in dem deine Bildschirmfotos gespeichert werden.

linux.copy.d8pickertitle = den Ordner wählen, in dem deine Bildschirmfotos gespeichert werden

linux.copy.desktopactivity = Desktop-Aktivität

linux.copy.desktopactivityisonwheneverthesolstoneapptak = Desktop-Aktivität ist an, wann immer die solstone-App deinen Bildschirm entgegennimmt, und Pausieren pausiert beides. die solstone-App nimmt die App, den Fenstertitel, Webadressen und die Zeit aktiv oder im Leerlauf entgegen, die du mit ihr teilst, und alles davon geht in dein Journal.

linux.copy.doesthismatchyourjournal = stimmt das mit deinem Journal überein?

linux.copy.drawcursor = Cursor zeichnen

linux.copy.enterawholenumbergreaterthan0 = gib eine ganze Zahl größer als 0 ein.

linux.copy.foundjournalthiscomputer = dein Journal auf diesem Computer gefunden

linux.copy.from = aus { $folder }

linux.copy.getafreshlink = frischen Link holen

linux.copy.gethelp = Hilfe holen

linux.copy.healthdetails = Details zum Zustand

linux.copy.help = Hilfe

linux.copy.helpreport = Problem melden

linux.copy.intake = Annahme

linux.copy.intakeisntrunning = die Annahme läuft nicht.

linux.copy.intakeispauseduntilyouresume = die Annahme ist pausiert, bis du fortsetzt.

linux.copy.intakeisrunning = die Annahme läuft.

linux.copy.journal = journal

linux.copy.journal3 = dein Journal

linux.copy.journaldetails = Details zum Journal

linux.copy.journallinkinvalidbody = kopiere den vollständigen Link aus deinem Chat und öffne ihn erneut.

linux.copy.journallinkinvalidtitle = dieser Journal-Link ist nicht gültig.

linux.copy.journalneedsupdatebeforeit = dein Journal braucht eine Aktualisierung, bevor es diese Notizen entgegennehmen kann. sie warten auf diesem Computer.

linux.copy.journalsetupaction = ein Journal auf diesem Computer einrichten

linux.copy.journalsetupfailed = dein Journal konnte nicht vorbereitet werden. führe { $literal1 } in einem Terminal aus, um zu sehen, was Aufmerksamkeit braucht.

linux.copy.journalsetupinbrowser = schließ die Ersteinrichtung in deinem Browser ab.

linux.copy.journalsetupinstall = das Journal installieren

linux.copy.journalsetupnotinstalled = das Journal ist auf diesem Computer noch nicht installiert.

linux.copy.journalsetupopenfailed = dein Journal ist vorbereitet, aber der Browser hat sich nicht geöffnet. führe { $literal1 } in einem Terminal aus, um die Ersteinrichtung abzuschließen.

linux.copy.journalsetuppreparing = dein Journal wird vorbereitet…

linux.copy.keepdesktopactivityonwhilepaused = Desktop-Aktivität beim Pausieren an lassen

linux.copy.lastchecked = zuletzt geprüft

linux.copy.lastdelivery = letzte Zustellung

linux.copy.lastdeliveryvalue = letzte Zustellung: { $value }

linux.copy.linuxdoesnotclassifyprivatewindows = Linux stuft private Fenster nicht ein.

linux.copy.mark = mark

linux.copy.markunavailable = mark nicht verfügbar

linux.copy.moveit = es verschieben

linux.copy.movesolstonetmux = solstone für { $literal1 } hereinverschieben?

linux.copy.needahandreachahumanwerehappytohelp = Hilfe nötig? meld dich. wir helfen gern.

linux.copy.needattention = brauchen Aufmerksamkeit

linux.copy.needsattention = braucht Aufmerksamkeit

linux.copy.newpairingcancelled = neue Kopplung abgebrochen

linux.copy.newpairingcancelledyourcurrentpairingstaysin = neue Kopplung abgebrochen. deine aktuelle Kopplung bleibt in Verwendung.

linux.copy.nologfilesareavailableyettryagainafterusing = noch keine Protokolldateien. versuch es noch einmal, nachdem du die App genutzt hast.

linux.copy.norecentlogsareavailableyettryagainafterusi = noch keine neueren Protokolle. versuch es noch einmal, nachdem du die App genutzt hast.

linux.copy.not = nicht

linux.copy.nothingthiscomputerhaskept = nichts, was dieser Computer behalten hat, ist über diesen Link in dieses Journal gegangen. du hast vielleicht den falschen Link eingefügt, oder etwas stimmt nicht. hol einen neuen Kopplungslink aus deinem Journal und versuch es noch einmal, oder schreib an { $literal1 }, und wir helfen.

linux.copy.nothingwaitinggoesintoyourjournaluntilyoudo = nichts, was wartet, geht in dein Journal, bis du es tust.

linux.copy.notpaired = nicht gekoppelt

linux.copy.notreachingyourjournalrightnow = erreicht dein Journal gerade nicht

linux.copy.notthisdevice = nicht auf diesem Gerät

linux.copy.notyet = noch nicht

linux.copy.obsidian = { $literal1 }

linux.copy.obsidianwarning = dein Journal hat schon einen Tresor namens { $vault } von einem anderen Gerät. wenn du fortfährst, gehen Änderungen von beiden Geräten in dein Journal.

linux.copy.obsidianwarningdate = dein Journal hat schon einen Tresor namens { $vault } von einem anderen Gerät, zuletzt aktualisiert { $date }. wenn du fortfährst, gehen Änderungen von beiden Geräten in dein Journal.

linux.copy.obsidianwarningdevice = dein Journal hat schon einen Tresor namens { $vault } von { $device }. wenn du fortfährst, gehen Änderungen von beiden Geräten in dein Journal.

linux.copy.obsidianwarningdevicedate = dein Journal hat schon einen Tresor namens { $vault } von { $device }, zuletzt aktualisiert { $date }. wenn du fortfährst, gehen Änderungen von beiden Geräten in dein Journal.

linux.copy.off = aus

linux.copy.offmeansnonethreegoes = aus heißt, dass keines der drei in dein Journal geht, was auch immer dein Desktop tut.

linux.copy.ofinyourjournal = { $done } von { $total } in deinem Journal

linux.copy.ofscreenshotsinyourjournal = { $done } von { $total } Bildschirmfotos in deinem Journal

linux.copy.omittedbytes = { $count ->
    [one] { $break }[ausgelassen { $formatted } Byte]{ $break }
   *[other] { $break }[ausgelassen { $formatted } Bytes]{ $break }
}

linux.copy.on = an

linux.copy.openthenetworkappinyourjournalandgetapairi = öffne die Netzwerk-App in deinem Journal und hol einen Kopplungslink.

linux.copy.openyourjournal = dein Journal öffnen

linux.copy.otherfilesinstaywheretheyare = andere Dateien in { $folder } bleiben, wo sie sind.

linux.copy.pairadevice = Gerät koppeln

linux.copy.pairagain = erneut koppeln

linux.copy.pairing = koppelt…

linux.copy.pairingcancelledgetafreshpairinglinkfromyour = Kopplung abgebrochen. hol einen neuen Kopplungslink aus deinem Journal, wenn du soweit bist.

linux.copy.pairingdidntgothroughgetanewpairinglinkfro = die Kopplung ist nicht zustande gekommen. hol einen neuen Kopplungslink aus deinem Journal und versuch es noch einmal.

linux.copy.pairinglink = Kopplungslink

linux.copy.partthisvaultcant = ein Teil dieses Tresors kann nicht gelesen werden. prüfe die Berechtigungen der Ordner darin. der Rest des Tresors geht weiter in dein Journal.

linux.copy.pasteapairinglinkfromyourjournal = füge einen Kopplungslink aus deinem Journal ein.

linux.copy.pastepairinglink = Kopplungslink einfügen

linux.copy.pause = pausieren

linux.copy.paused = pausiert

linux.copy.pausing = pausiert…

linux.copy.presented = vorgelegt

linux.copy.quit = solstone beenden

linux.copy.quitfailed = die solstone-App konnte nicht beendet werden. versuch es noch einmal.

linux.copy.readinglogs = liest Protokolle…

linux.copy.readrecentlogs = neuere Protokolle lesen

linux.copy.readsettingsagain = Einstellungen erneut lesen

linux.copy.readthelogsbelowthenchoosewhethertosaveaco = lies die Protokolle unten und wähle dann, ob du eine Kopie speicherst.

linux.copy.readthesolstoneappsrecentlogsbeforesavinga = lies die neueren Protokolle der solstone-App, bevor du eine Kopie für einen Support-Bericht speicherst. von hier wird nichts gesendet.

linux.copy.recentlogs = letzte Protokolle

linux.copy.restarting = startet neu…

linux.copy.restartneeded = Neustart nötig

linux.copy.restartthesolstoneapp = die solstone-App neu starten

linux.copy.resume = fortsetzen

linux.copy.resuming = setzt fort…

linux.copy.s1 = Bildschirmfotos

linux.copy.s10counting = wird geprüft…

linux.copy.s10today = ab heute starten

linux.copy.s11 = wähle, wie weit zurück es anfangen soll, und hole sie dann herein.

linux.copy.s12 = deine Bildschirmfotos konnten nicht gezählt werden.

linux.copy.s14 = ein Bildschirmfoto, das schon in deinem Journal ist, bleibt dort, wenn du es aus dem Ordner löschst.

linux.copy.s15 = was schon in deinem Journal ist, bleibt, wenn du pausierst oder das ausschaltest.

linux.copy.s16computer = Bildschirmfotos, die du machst, während es pausiert ist, kommen nicht herein.

linux.copy.s17 = Bildschirmfotos, die schon in deinem Journal sind, werden nicht erneut hereingeholt.

linux.copy.s19 = wartet darauf, in dein Journal zu gehen

linux.copy.s21 = wartet auf eine Aktualisierung des Journals. deine Bildschirmfotos warten auf diesem Computer.

linux.copy.s22 = Ordner

linux.copy.s23 = es kommt nur herein, was die solstone-App als Bildschirmfoto erkennt.

linux.copy.s7 = von vor heute

linux.copy.s9 = du siehst, wie viele Bildschirmfotos es gibt, bevor welche hereinkommen.

linux.copy.saveacopy = Kopie speichern…

linux.copy.savesettings = Einstellungen speichern

linux.copy.saving = speichert…

linux.copy.savingcopy = speichert die Kopie…

linux.copy.savingwritesexactlythepreviewabove = speichern schreibt genau die Vorschau oben.

linux.copy.screen = Bildschirm

linux.copy.screenandaudioarepauseduntilyouresumedesktop = Bildschirm und Audio sind pausiert, bis du fortsetzt. die Desktop-Aktivität läuft weiter.

linux.copy.screenandaudiostillpausedesktopactivitykeeps = Bildschirm und Audio pausieren weiter. die Desktop-Aktivität läuft weiter.

linux.copy.screenaudiodesktopactivity = Bildschirm, Audio und Desktop-Aktivität

linux.copy.screenintakeiswaiting = Bildschirm-Annahme wartet

linux.copy.screenshotslookback76 = die letzten 30 Tage

linux.copy.screenshotslookback77 = die letzten 6 Monate

linux.copy.screenshotslookback78 = das letzte Jahr

linux.copy.screenshotslookback79 = alles

linux.copy.screenshotslookback80 = nichts von vor heute

linux.copy.seconds = Sekunden

linux.copy.segmentinterval = Segmentintervall

linux.copy.sentfoundnotesjournal = { $sent } von { $found } Notizen in deinem Journal

linux.copy.settings = Einstellungen

linux.copy.settingsactivityonlyhelp = nur Desktop-Aktivität, mit Bildschirm und Ton aus.

linux.copy.settingssaved = Einstellungen gespeichert.

linux.copy.solstoneappcantread = die solstone-App kann diesen Tresor nicht lesen. prüfe, dass sein Ordner da ist und dass das Laufwerk, auf dem er liegt, verbunden ist. in deinem Journal ändert sich nichts, solange er nicht erreichbar ist.

linux.copy.solstoneappcouldntsave = die solstone-App konnte ihre Stelle für diesen Tresor nicht speichern. prüfe, dass auf diesem Computer Platz frei ist.

linux.copy.solstoneappdidntfind = die solstone-App hat auf diesem Computer keinen Tresor gefunden. wähle stattdessen seinen Ordner.

linux.copy.solstoneapptakesnotesone = die solstone-App nimmt die Notizen in einem Tresor auf diesem Computer entgegen. alle gehen in dein Journal, und danach jede, sobald sie sich ändert. dein Tresor bleibt, wie er ist.

linux.copy.somenotescouldntgo = einige Notizen haben es nicht in dein Journal geschafft. die solstone-App versucht sie beim nächsten Start erneut.

linux.copy.somethingansweredinsteadjournal = etwas hat statt deines Journals geantwortet

linux.copy.somethingansweredthiscomputerat = unter { $address } hat etwas diesem Computer geantwortet, und es ist nicht das Journal, mit dem dieser Computer gekoppelt ist. das kann ein anderes Gerät sein, das diese Adresse jetzt nutzt, oder dein Journal, wenn seine Identität zurückgesetzt wurde. wenn die mark unter "was geantwortet hat" zu der passt, die dein Journal jetzt zeigt, kopple diesen Computer erneut.

linux.copy.somethingansweredthiscomputerthrough = etwas hat diesem Computer über das Relay geantwortet, und es ist nicht das Journal, mit dem dieser Computer gekoppelt ist. das ist weniger gewiss als eine direkte Verbindung.

linux.copy.starting = wird gestartet…

linux.copy.startpaused = pausiert starten

linux.copy.startthesolstoneapp = die solstone-App starten

linux.copy.statusunavailable = Status nicht verfügbar

linux.copy.syncingjournal = wird mit deinem Journal abgeglichen…

linux.copy.takestmuxsessionsallit = nimmt deine { $literal1 }-Sitzungen entgegen, und alles davon geht in dein Journal. braucht { $literal1 } auf diesem Computer und funktioniert über { $literal2 } und ohne Desktop.

linux.copy.terminal = Terminal

linux.copy.thatdoesntmatch = das stimmt nicht überein

linux.copy.thatfolderisntobsidian = dieser Ordner ist kein { $literal1 }-Tresor, deshalb hat die solstone-App ihn nicht hinzugefügt. wähle den eigenen Ordner des Tresors.

linux.copy.thatfoldersnamecan = der Name dieses Ordners kann in deinem Journal kein Tresorname sein. benenne den Ordner um und versuch es noch einmal.

linux.copy.thatpairinglinkisntonethesolstoneappcanrea = diesen Kopplungslink kann die solstone-App nicht lesen. hol einen neuen Kopplungslink aus deinem Journal und versuch es noch einmal.

linux.copy.thefolderisntonthiscomputer = der Ordner { $folder } ist nicht mehr auf diesem Computer.

linux.copy.thelogpreviewwaslimitedmorelogtextmaybeava = { $ws1 }[die Protokollvorschau war begrenzt; mehr Protokolltext ist vielleicht verfügbar]{ $ws1 }

linux.copy.thepairingchangedwhileyouwerecheckingitsmark = die Kopplung hat sich geändert, während du ihre mark geprüft hast. prüfe die aktuelle mark, bevor du bestätigst.

linux.copy.thepairingwindowclosedgetanewpairinglinkfro = das Kopplungsfenster ist zu. hol einen neuen Kopplungslink aus deinem Journal und versuch es noch einmal.

linux.copy.thesetakeeffectthenexttimethesolstoneappsta = das gilt beim nächsten Start der solstone-App.

linux.copy.thesolstoneappcannowconnecttoyourjournal = die solstone-App kann sich jetzt mit deinem Journal verbinden.

linux.copy.thesolstoneapphasntansweredyetcheckforades = die solstone-App hat noch nicht geantwortet. sieh nach einer Desktop-Freigabe und prüfe dann den Status.

linux.copy.thesolstoneapprestartedwithyoursavedsettings = die solstone-App ist mit deinen gespeicherten Einstellungen neu gestartet.

linux.copy.thewindowcouldntstartopenxfordetails = das Fenster konnte nicht starten. öffne { $path } für Details.

linux.copy.thewindowcouldntstarttryopeningitagain = das Fenster konnte nicht starten. versuch, es erneut zu öffnen.

linux.copy.thewindowneedsgtk46ornewercouldntloadits = das Fenster braucht { $literal1 } 4.6 oder neuer. seine Desktop-Bibliotheken konnten nicht geladen werden. öffne { $path } für Details.

linux.copy.thisisjournal = das ist dein Journal

linux.copy.thismovessolstonetmuxinto = das verschiebt solstone für { $literal1 } in diese App, und es endet endgültig. was noch wartet, geht über diese App in dein Journal, und was nicht gehen kann, bleibt auf diesem Computer. wenn diese App noch nicht gekoppelt ist, koppelt sie mit dem Journal, das solstone für { $literal1 } nutzt.

linux.copy.thisstartsthesolstoneappnowandatsigninopen = das startet die solstone-App jetzt und bei der Anmeldung. dieses Fenster allein zu öffnen startet die Annahme nicht.

linux.copy.toolargebring = zu groß zum Hereinholen

linux.copy.tryagain = erneut versuchen

linux.copy.turningthisoffstopsbringing = wenn du das ausschaltest, kommen keine Änderungen mehr herein. was schon in deinem Journal ist, bleibt dort, und was wartet, bleibt auf diesem Computer, bis du es wieder einschaltest.

linux.copy.turnoff = ausschalten

linux.copy.unavailable = nicht verfügbar

linux.copy.updateshow = mit automatischen Aktualisierungen an aktualisiert sich die App zusammen mit deinem Journal, wenn das solstone-Installationsprogramm beide auf diesem Computer eingerichtet hat. sonst führe das solstone-Installationsprogramm erneut mit --upgrade aus.

linux.copy.updatesjournalversion = Journalversion

linux.copy.usethisvault = diesen Tresor nutzen

linux.copy.vault = Tresor

linux.copy.vaultsthiscomputer = Tresore auf diesem Computer

linux.copy.vaultwiththisnameis = ein Tresor mit diesem Namen ist schon in deinem Journal

linux.copy.versionvalue = App-Version: { $value }

linux.copy.viewdetails = Details anzeigen

linux.copy.waitingforthesolstoneapptoanswerifyourdeskt = wartet auf die Antwort der solstone-App. wenn dein Desktop fragt, was du teilst, triff die Wahl dort.

linux.copy.waitingforyoutoconfirmyourjournalsmark = wartet darauf, dass du die mark deines Journals bestätigst

linux.copy.waitingjournalupdate = wartet auf eine Aktualisierung des Journals

linux.copy.waitingreachjournal = wartet darauf, dein Journal zu erreichen

linux.copy.waitingtosync = wartet auf den Abgleich

linux.copy.waitingvalue = wartet auf den Abgleich: { $value }

linux.copy.webaddressesturnsondesktopaccessibility = Webadressen (schaltet die Desktop-Barrierefreiheit ein)

linux.copy.whatanswered = was geantwortet hat

linux.copy.windowtitles = Fenstertitel

linux.copy.windowtitlesleftoutofdesktopactivity = Fenstertitel, die bei der Desktop-Aktivität ausgelassen sind

linux.copy.yesthisismyjournal = ja, das ist mein Journal

linux.copy.your = dein

linux.copy.yourcurrentpairingstaysinuseuntilyouconfirm = deine aktuelle Kopplung bleibt in Verwendung, bis du diese mark bestätigst.

linux.copy.yourjournalnotsetupyet = dein Journal, noch nicht eingerichtet

linux.copy.yourjournalsmarkisconfirmedbutthesolstoneap = die mark deines Journals ist bestätigt, aber die solstone-App konnte nicht neu starten. versuch es noch einmal.

linux.copy.yourjournalsmarkisconfirmedbutthesolstoneap2 = die mark deines Journals ist bestätigt, aber die solstone-App braucht Aufmerksamkeit. prüfe die Details und versuch es noch einmal.

linux.copy.yourjournalsmarkunavailablerightnow = die mark deines Journals, gerade nicht verfügbar

linux.copy.yoursettingsaresavedbutthesolstoneappcouldn = deine Einstellungen sind gespeichert, aber die solstone-App konnte nicht neu starten. versuch es noch einmal.

linux.copy.yoursettingsaresavedrestartthesolstoneappto = deine Einstellungen sind gespeichert. starte die solstone-App neu, um sie jetzt zu nutzen.

linux.copy.yoursettingschangedoutsidethiswindowreadthem = deine Einstellungen haben sich außerhalb dieses Fensters geändert. lies sie erneut, bevor du speicherst.

linux.copy.yousaidthismarkdoesntmatchtheoneyourjourna2 = du hast gesagt, diese mark stimmt nicht mit der überein, die dein Journal zeigt. deine aktuelle Kopplung bleibt in Verwendung. hol einen neuen Kopplungslink aus deinem Journal, oder schreib an { $literal1 }, und wir helfen.

linux.desktop.clipboardcommandexitedunsuccessfully = Zwischenablage-Befehl ohne Erfolg beendet

linux.desktop.couldnotopenyourjournalwaitforthesolstoneap = Dein Journal konnte nicht geöffnet werden. Warte, bis die solstone-App wieder verbunden ist, und versuch es noch einmal.

linux.desktop.couldntcopyrunsolstonelinuxabouttoselectand = kopieren ist nicht gelungen. führe `{ $literal1 } about` aus, um deine Versionen auszuwählen und zu kopieren.

linux.desktop.couldntcopywithoutxinstallitorrunsolstonel = ohne { $value1 } ist kopieren nicht gelungen. installiere es, oder führe `{ $literal1 } about` aus, um deine Versionen auszuwählen und zu kopieren.

linux.desktop.observercommandmustberoutedbytherunloopowne = der Observer-Befehl muss vom Eigentümer der Run-Loop geleitet werden

linux.doctor.cantreachyourdesktopxlogoutandbackintore = dein Desktop ist nicht erreichbar ({ $value1 }). melde dich ab und wieder an, um ihn wiederherzustellen

linux.doctor.couldnotreachthesessionbustocheckthepaneli = der Session-Bus war nicht erreichbar, um das Panel-Symbol zu prüfen

linux.doctor.couldnotreadloginsessionsx = Anmeldesitzungen konnten nicht gelesen werden: { $error }

linux.doctor.couldnotstarttheprobex = die Probe konnte nicht starten: { $error }

linux.doctor.displaynotsetstartsolstonelinuxfromthex11gr = { $literal1 } nicht gesetzt. starte { $literal2 } aus der grafischen { $literal3 }-Sitzung

linux.doctor.doctor9checksxfailedxwarnings = { $ws1 }doctor: 9 Prüfungen, { $failures } fehlgeschlagen, { $warnings } Warnungen

linux.doctor.followingsessionxwayland = folgt der Sitzung { $session } (wayland)

linux.doctor.followingsessionxx11onx = folgt der Sitzung { $session } (x11 auf { $display })

linux.doctor.gnomeneedsanextensionrunsolstonelinuxpanelic = { $literal1 } braucht eine Erweiterung. führe { $literal2 } aus

linux.doctor.gstreamerelementsxmissinginstallthepipewirea = { $literal1 }-Element(e) { $value1 } fehlen. installiere die Pakete { $literal2 } und good/base

linux.doctor.gstreamerinitializationfailedxinstallgstreamer = { $literal1 }-Initialisierung fehlgeschlagen: { $error }. installiere { $literal1 } 1.x

linux.doctor.label.desktopsession = Desktop-Sitzung

linux.doctor.label.fail = fail

linux.doctor.label.gstreamer = gstreamer

linux.doctor.label.na = n/a

linux.doctor.label.ok = ok

linux.doctor.label.panelicon = Panel-Symbol

linux.doctor.label.portal = xdg-desktop-portal

linux.doctor.label.pulse = pipewire (pulse)

linux.doctor.label.sessiontype = Sitzungstyp

linux.doctor.label.synchealth = Abgleich-Zustand

linux.doctor.label.systemduser = systemd --user

linux.doctor.label.warn = warn

linux.doctor.label.x11capture = x11-Aufnahme

linux.doctor.nodesktopsessionofyoursonthis = keine Desktop-Sitzung von dir auf diesem Computer. Bildschirm und Audio brauchen eine

linux.doctor.nodesktopsessionofyoursonthiscomputernothing = keine Desktop-Sitzung von dir auf diesem Computer. Bildschirm und Audio warten, bis du dich hier anmeldest (eine Sitzung, bei der du dich aus der Ferne anmeldest, zählt nicht)

linux.doctor.notapplicablenognomeshellonthesessionbus = nicht zutreffend (keine { $literal1 }-Shell am Session-Bus)

linux.doctor.notapplicablenox11display = nicht zutreffend (kein { $literal1 }-Display)

linux.doctor.notapplicablewaylandsession = nicht zutreffend (wayland-Sitzung)

linux.doctor.notregisterednotneededonx11usingximagesrc = nicht registriert — nicht nötig auf { $literal1 } (nutzt { $literal2 })

linux.doctor.okbrowserregistrationchromechromiumedgeandfir = ok{ $ws1 }Browser-Registrierung{ $ws2 }Chrome, Chromium, Edge und Firefox

linux.doctor.okfirefoxsnapallowedtostartthesolstoneapp = ok{ $ws1 }Firefox (snap){ $ws2 }darf die solstone-App starten

linux.doctor.okfirefoxsnapasksyouoncetoletitstarttheso = ok{ $ws1 }Firefox (snap){ $ws2 }fragt dich einmal, ob es die solstone-App starten darf

linux.doctor.orgfreedesktopportaldesktopnotregisteredon = { $literal1 } nicht am Session-Bus registriert. installiere oder starte { $literal2 }

linux.doctor.orgfreedesktopportaldesktopregisteredonsession = { $literal1 } am Session-Bus registriert

linux.doctor.pipewiresrcvp8encandwebmmuxavailable = pipewiresrc, vp8enc und webmmux verfügbar

linux.doctor.probepanickedinspectservicelogsandretry = Probe-Panic. sieh in die Dienstprotokolle und versuch es erneut

linux.doctor.pulseaudiocompatibleserverreachable = { $literal1 }-kompatibler Server erreichbar

linux.doctor.pulseaudiocompatibleserverunreachablexstartpip = { $literal1 }-kompatibler Server nicht erreichbar: { $error }. starte { $literal2 } Pulse oder { $literal1 }

linux.doctor.screenaudioanddesktopactivityareoff = Bildschirm, Audio und Desktop-Aktivität sind aus

linux.doctor.sessionbusprobefailedxstartagraphicalsession = Session-Bus-Probe fehlgeschlagen: { $error }. starte eine grafische Sitzung und sieh nach, dass { $literal1 } installiert ist

linux.doctor.sessionbusunreachablexstartagraphicalsession = Session-Bus nicht erreichbar: { $error }. starte eine grafische Sitzung und sieh nach, dass { $literal1 } installiert ist

linux.doctor.systemctlusernotreachableruninsideasystemdus = { $literal1 } nicht erreichbar. führe es in einer systemd-Benutzersitzung aus

linux.doctor.systemctlusernotreachablexruninsideasystemd = { $literal1 } nicht erreichbar: { $error }. führe es in einer systemd-Benutzersitzung aus

linux.doctor.thegnomeextensionisinstalledbutoffrunsolston = die Erweiterung { $literal1 } ist installiert, aber aus. führe { $literal2 } aus

linux.doctor.thegnomeextensionisonbutnopaneliconyetlog = die Erweiterung { $literal1 } ist an, aber noch kein Panel-Symbol. melde dich ab und wieder an, wenn es so bleibt

linux.doctor.thepaneliconextensionisinstalledlogoutandba = die Panel-Symbol-Erweiterung ist installiert. melde dich ab und wieder an, um abzuschließen

linux.doctor.thisdesktopcanshowthepanelicon = dieser Desktop kann das Panel-Symbol zeigen

linux.doctor.timedoutafter2schecktheuserdbussessionand = Zeitüberschreitung nach 2 s. prüfe die D-Bus-Benutzersitzung und { $literal1 }

linux.doctor.unrecognizedsessiontypexwaylandorx11required = Sitzungstyp '{ $value }' nicht erkannt. { $literal1 } oder { $literal2 } nötig

linux.doctor.warnbrowserregistrationmissingoroutofdatefor = warn{ $ws1 }Browser-Registrierung{ $ws2 }fehlt oder ist veraltet für { $value1 }. starte die solstone-App, um sie erneut zu schreiben

linux.doctor.warnchromiumsnapcantreachthesolstoneappasn = warn{ $ws1 }Chromium (snap){ $ws2 }erreicht die solstone-App nicht. ein snap von Chromium kann sie nicht starten. nimm Chrome, Edge oder Firefox

linux.doctor.warnfirefoxsnapasksyouoncetoletitstartthe = warn{ $ws1 }Firefox (snap){ $ws2 }fragt dich einmal, ob es die solstone-App starten darf. doctor konnte nicht lesen, was du geantwortet hast

linux.doctor.warnfirefoxsnapyoutoldyourdesktopnottoletf = warn{ $ws1 }Firefox (snap){ $ws2 }du hast deinem Desktop gesagt, Firefox die solstone-App nicht starten zu lassen, deshalb wird von dort nichts entgegengenommen. um erneut gefragt zu werden, führe aus:{ $ws3 }gdbus { $value1 } --method { $literal4 } { $PERMISSION-TABLE } { $value2 } { $SNAP-FIREFOX }

linux.doctor.warnxcantreachthesolstoneappfrominsideafl = warn{ $ws1 }{ $id }{ $ws1 }erreicht die solstone-App aus einem flatpak nicht. nimm Chrome, Edge oder Firefox, außerhalb von flatpak installiert

linux.doctor.wayland = wayland

linux.doctor.x11randrandximagesrcavailable = { $literal1 } RandR und { $literal2 } verfügbar

linux.doctor.x11randrunavailablexcheckdisplayandxserverr = { $literal1 } RandR nicht verfügbar: { $error }. prüfe { $literal2 } und die RandR-Unterstützung des X-Servers

linux.doctor.x11usingximagesrccapture = x11 (mit { $literal1 }-Aufnahme)

linux.doctor.xcheckthatsystemdlogindisrunning = { $value1 }. prüfe, dass { $literal1 } läuft

linux.doctor.xdgsessiontypenotsetwaylandor = { $literal1 } nicht gesetzt. { $literal2 } oder { $literal3 } nötig

linux.doctor.ximagesrcmissinginstallthegstreamergoodplugins = { $literal1 } fehlt. installiere das Paket { $literal2 } good plugins

linux.doctor.yourdesktopsessionisnttheoneinfrontofthes = deine Desktop-Sitzung ist nicht die vor dem Bildschirm. Bildschirm und Audio warten, bis sie es ist

linux.health.idleconfirmingwithyourjournal = Leerlauf, bestätigt mit deinem Journal

linux.health.idleconnected = Leerlauf, verbunden

linux.health.idleconnecting = Leerlauf, verbindet

linux.health.idleconnectionunavailableheldonthisdevice = Leerlauf, Verbindung nicht verfügbar (auf diesem Gerät zurückgehalten)

linux.health.idleconnectionunavailableheldonthisdevice2 = Leerlauf, Verbindung nicht verfügbar, auf diesem Gerät zurückgehalten

linux.health.idlenostatus = Leerlauf, kein Status

linux.health.idleofflineheldonthisdevice = Leerlauf, offline (auf diesem Gerät zurückgehalten)

linux.health.idleofflineheldonthisdevice2 = Leerlauf, offline, auf diesem Gerät zurückgehalten

linux.health.idlepairagain = Leerlauf, erneut koppeln

linux.health.idlepairingnotsaved = Leerlauf, Kopplung nicht gespeichert

linux.health.idlepairingrequired = Leerlauf, Kopplung nötig

linux.health.idlepairingunsafe = Leerlauf, Kopplung unsicher

linux.health.idlesyncing = Leerlauf, Abgleich

linux.health.idlesyncuptodate = Leerlauf, Abgleich auf dem neuesten Stand

linux.health.idleupdaterequired = Leerlauf, Aktualisierung nötig

linux.health.journalmark = dein Journal:{ $ws1 }{ $paired }

linux.health.markclaim = { $spoken }{ $ws1 }({ $claim })

linux.health.markclaimed = behauptet, nicht überprüft

linux.health.n0pending = 0 ausstehend

linux.health.noidentitypresented = keine Identität vorgelegt

linux.health.onconfirmingwithyourjournal = an, bestätigt mit deinem Journal

linux.health.onconnected = an, verbunden

linux.health.onconnecting = an, verbindet

linux.health.onconnectionunavailableheldonthisdevice = an, Verbindung nicht verfügbar (auf diesem Gerät zurückgehalten)

linux.health.onconnectionunavailableheldonthisdevice2 = an, Verbindung nicht verfügbar, auf diesem Gerät zurückgehalten

linux.health.onnostatus = an, kein Status

linux.health.onofflineheldonthisdevice = an, offline (auf diesem Gerät zurückgehalten)

linux.health.onofflineheldonthisdevice2 = an, offline, auf diesem Gerät zurückgehalten

linux.health.onpairagain = an, erneut koppeln

linux.health.onpairingnotsaved = an, Kopplung nicht gespeichert

linux.health.onpairingrequired = an, Kopplung nötig

linux.health.onpairingunsafe = an, Kopplung unsicher

linux.health.onsyncing = an, Abgleich

linux.health.onsyncuptodate = an, Abgleich auf dem neuesten Stand

linux.health.onupdaterequired = an, Aktualisierung nötig

linux.health.pendingunconfirmed = ausstehend, unbestätigt

linux.health.sighting = { $header }:{ $ws1 }was geantwortet hat:{ $ws2 }{ $answer }{ $ws3 }{ $paired }{ $footer }

linux.health.syncconfirmingwithyourjournal = Abgleich: bestätigt mit deinem Journal

linux.health.syncconnecteduptodate = Abgleich: verbunden; auf dem neuesten Stand

linux.health.syncconnecting = Abgleich: verbindet

linux.health.syncconnectingwaitwhilethisdeviceconnectstoy = Abgleich: verbindet; warte, während dieses Gerät sich mit deinem Journal verbindet

linux.health.syncconnectionunavailableheldonthisdevice = Abgleich: Verbindung nicht verfügbar; auf diesem Gerät zurückgehalten

linux.health.syncconnectionunavailableheldonthisdevicerest = Abgleich: Verbindung nicht verfügbar; auf diesem Gerät zurückgehalten; starte die solstone-App neu; wenn das so bleibt, kopple dieses Gerät erneut

linux.health.syncconnectionunavailablerestartthesolstoneapp = Abgleich: Verbindung nicht verfügbar; starte die solstone-App neu; wenn das so bleibt, kopple dieses Gerät erneut

linux.health.synchealthconnectingwaitwhilethisdeviceconnec = Abgleich-Zustand: verbindet; warte, während dieses Gerät sich mit deinem Journal verbindet

linux.health.synchealthconnectionunavailablerestartthesolst = Abgleich-Zustand: Verbindung nicht verfügbar; starte die solstone-App neu; wenn das so bleibt, kopple dieses Gerät erneut

linux.health.synchealthofflineheldonthisdevicewillretry = Abgleich-Zustand: offline; auf diesem Gerät zurückgehalten; wird erneut versucht

linux.health.synchealthpairingnotsavedfixthisdevicespair = Abgleich-Zustand: Kopplung nicht gespeichert; korrigiere die Kopplungsberechtigungen dieses Geräts und starte dann die solstone-App neu

linux.health.synchealthpairingunsaferepairthisdevicespair = Abgleich-Zustand: Kopplung unsicher; stelle die Kopplung dieses Geräts wieder her und starte die solstone-App neu

linux.health.synchealthpairthisdevicewithyourjournal = Abgleich-Zustand: kopple dieses Gerät mit deinem Journal

linux.health.synchealthpairthisdevicewithyourjournalagain = Abgleich-Zustand: kopple dieses Gerät erneut mit deinem Journal

linux.health.synchealthupdaterequiredupdatethesolstoneapp = Abgleich-Zustand: Aktualisierung nötig; aktualisiere die solstone-App

linux.health.synchealthwaitwhilethisdeviceconfirmswithyou = Abgleich-Zustand: warte, während dieses Gerät mit deinem Journal bestätigt

linux.health.synchealthyourjournalisreceivingnotconfirmed = Abgleich-Zustand: Abgleich; von deinem Journal noch nicht bestätigt

linux.health.syncingfallback = Abgleich...

linux.health.syncnostatus = Abgleich: kein Status

linux.health.syncnostatusfromthesolstoneapprightnow = Abgleich: gerade kein Status von der solstone-App

linux.health.syncnostatusthesolstoneappisnotrunningorha = Abgleich: kein Status; die solstone-App läuft nicht, oder hat noch keinen Status

linux.health.syncofflineheldonthisdevicewillretry = Abgleich: offline; auf diesem Gerät zurückgehalten; wird erneut versucht

linux.health.syncofflinewillretry = Abgleich: offline; wird erneut versucht

linux.health.syncpairagain = Abgleich: erneut koppeln

linux.health.syncpairagainpairthisdevicewithyourjournala = Abgleich: erneut koppeln; kopple dieses Gerät erneut mit deinem Journal

linux.health.syncpairingnotsaved = Abgleich: Kopplung nicht gespeichert

linux.health.syncpairingnotsavedfixthisdevicespairingper = Abgleich: Kopplung nicht gespeichert; korrigiere die Kopplungsberechtigungen dieses Geräts und starte dann die solstone-App neu

linux.health.syncpairingrequired = Abgleich: Kopplung nötig

linux.health.syncpairingrequiredpairthisdevicewithyourjou = Abgleich: Kopplung nötig; kopple dieses Gerät mit deinem Journal

linux.health.syncpairingunsafe = Abgleich: Kopplung unsicher

linux.health.syncpairingunsaferepairthisdevicespairingand = Abgleich: Kopplung unsicher; stelle die Kopplung dieses Geräts wieder her und starte die solstone-App neu

linux.health.syncpairthisdevicewithyourjournal = Abgleich: kopple dieses Gerät mit deinem Journal

linux.health.syncpairthisdevicewithyourjournalagain = Abgleich: kopple dieses Gerät erneut mit deinem Journal

linux.health.syncprogress = Abgleich: { $progress }

linux.health.syncsyncingyourjournalisreceivingnotconfirmed = Abgleich: Abgleich; von deinem Journal noch nicht bestätigt

linux.health.syncupdaterequiredupdatethesolstoneapp = Abgleich: Aktualisierung nötig; aktualisiere die solstone-App

linux.health.syncupdatethesolstoneapp = Abgleich: aktualisiere die solstone-App

linux.health.syncuptodate = Abgleich: auf dem neuesten Stand

linux.health.syncwaitwhilethisdeviceconfirmswithyourjourn = Abgleich: warte, während dieses Gerät mit deinem Journal bestätigt

linux.health.syncwaitwhilethisdeviceconnectstoyourjournal = Abgleich: warte, während dieses Gerät sich mit deinem Journal verbindet

linux.health.thesolstoneappisnotrunningorhasnostatusyet = die solstone-App läuft nicht, oder hat noch keinen Status

linux.health.unknownjournalseenatx = unbekanntes Journal gesehen bei { $address }

linux.health.unknownjournalseenthroughtherelay = unbekanntes Journal über das Relay gesehen

linux.health.uptodateat = Abgleich-Zustand: auf dem neuesten Stand um { $timestamp }

linux.health.waitingforyoutoconfirmyourjournalsmarknothi = warte auf deine Bestätigung der mark deines Journals. nichts, was wartet, geht in dein Journal, bevor du es tust.{ $ws1 }wenn du soweit bist, führe aus: { $literal2 }

linux.import.allhistory = der gesamte Importverlauf in deinem Journal

linux.import.attention = braucht Aufmerksamkeit

linux.import.cannotconvert = dieses Gerät kann dieses Foto nicht umwandeln

linux.import.delivered = in deinem Journal.

linux.import.expired = abgelaufen

linux.import.explanation = fügt jede Datei hinzu, die du hier wählst oder auf dieses Fenster ablegst. dein Journal findet heraus, was es ist.

linux.import.explanationnodrop = fügt jede Datei hinzu, die du hier wählst. dein Journal erkennt, was es ist.

linux.import.filekind = dein Journal kann diese Art von Datei nicht entgegennehmen

linux.import.filemissing = die Kopie auf diesem Gerät fehlt

linux.import.filenotread = das konnte nicht gelesen werden. füge es erneut hinzu

linux.import.filetoolarge = es ist größer, als dein Journal entgegennimmt

linux.import.held = geht gerade nicht in dein Journal. jedes sagt, warum.

linux.import.inyourjournalrel = in deinem Journal · { $when }

linux.import.journalrefusal = dein Journal konnte das nicht annehmen.

linux.import.moreimports = weitere Importe

linux.import.moreimportssub = Anleitungen, Optionen und der Importverlauf in deinem Journal

linux.import.namedrefusal = { $name }: { $reason }

linux.import.nfiles = Dateien: { $count }

linux.import.noroom = auf diesem Gerät ist nicht genug Platz

linux.import.nothingwassaved = nichts wurde auf diesem Gerät gespeichert.

linux.import.nothingyet = noch nichts.

linux.import.nreadyinjournal = in deinem Journal bereit: { $count }

linux.import.nwaiting = wartend: { $count }

linux.import.pairagain = dein Journal nimmt dieses Gerät nicht mehr an. Importe warten hier, bis du erneut koppelst.

linux.import.readytoreview = bereit zum Prüfen

linux.import.readytoreviewcount = bereit zum Prüfen: { $count }

linux.import.recentrow.a11y = { $name }, in deinem Journal, { $when }. in deinem Journal öffnen.

linux.import.remove.a11y = { $name } entfernen

linux.import.removeerror = dies konnte nicht entfernt werden. versuch es noch einmal.

linux.import.removereadytoreview = das entfernt die Kopie auf diesem Gerät. die wartende Kopie in deinem Journal bleibt dort, bis du sie verwirfst oder sie abläuft.

linux.import.reviewdetail = in deinem Journal gespeichert. Vorschau anzeigen und bestätigen, um zu starten.

linux.import.reviewdetailhour = in deinem Journal gespeichert. prüfe innerhalb einer Stunde, bevor dein Journal die wartende Kopie entfernt.

linux.import.reviewexpireddetail = die wartende Kopie in deinem Journal ist abgelaufen. dieses Gerät hält die Datei noch.

linux.import.reviewinjournal = in deinem Journal prüfen

linux.import.reviewinjournal.a11y = { $name } in deinem Journal prüfen

linux.import.saved = auf diesem Gerät gespeichert: { $count }

linux.import.saving = speichert auf diesem Gerät: { $saved } von { $total }

linux.import.sendagain = erneut zum Prüfen senden

linux.import.sendagain.a11y = { $name } erneut zum Prüfen senden

linux.import.sending = sendet

linux.import.sendingellipsis = wird an dein Journal gesendet…

linux.import.sendingtojournal = sendet an dein Journal.

linux.import.statusneedsyou = Importe brauchen dich

linux.import.statusneedsyousub = öffne den Import, um zu sehen, warum.

linux.import.statusreadytoreview = { $count ->
    [one] { $display } Import bereit zum Prüfen
   *[other] { $display } Importe bereit zum Prüfen
}

linux.import.statusreadytoreviewsub = öffne den Import, um zu prüfen und zu starten.

linux.import.statusrow.a11y = Import, { $detail }.

linux.import.steperror = dieser Schritt konnte nicht fertig werden. versuch es noch einmal.

linux.import.storeerror = die Importe auf diesem Gerät konnten nicht gelesen werden. nichts wurde entfernt. starte die solstone-App neu, um es noch einmal zu versuchen.

linux.import.tile.a11y = Import, { $state }. { $detail }.

linux.import.trynow.a11y = { $name } jetzt versuchen

linux.import.waitforjournal = Importe warten auf diesem Gerät, bis du ein Journal verbindest.

linux.import.waitformark = Importe warten, bis du die mark deines Journals bestätigst.

linux.import.waitforupdate = Importe warten, bis dein Journal aktualisiert ist.

linux.import.waiting = wartet

linux.import.waitingdetail = wartet darauf, in dein Journal zu gehen

linux.import.waitingforjournal = wartet darauf, in dein Journal zu gehen.

linux.import.waitingforjournalshort = wartet auf dein Journal

linux.import.waitingforyourreview = wartet auf deine Prüfung

linux.import.waitingreconnect = wartet, bis dein Journal wieder verbunden ist.

linux.journalmark.journal = journal: { $value1 }·{ $value2 }

linux.journalmark.journaljournal = journal: dein Journal

linux.journalmark.journalnotpaired = journal: nicht gekoppelt

linux.journalmark.journalsmarkunavailableright = mark deines Journals: gerade nicht verfügbar

linux.journalmark.markwordsyougavedon = die Wörter der mark, die du angegeben hast, stimmen nicht mit der mark des Journals überein, deshalb ist dieser Computer nicht gekoppelt. prüfe die Wörter, hol einen neuen Kopplungslink aus deinem Journal und kopple erneut.

linux.journalmark.markwordsyougavedon2 = die Wörter der mark, die du angegeben hast, stimmen nicht mit der mark des Journals überein.

linux.journalmark.notchangedcurrentpairingstays = nicht geändert. deine aktuelle Kopplung bleibt in Verwendung. nichts, was dieser Computer behalten hat, ist über diesen Link in dieses Journal gegangen.

linux.journalmark.notpairedgetfreshpairing = nicht gekoppelt. hol einen neuen Kopplungslink aus deinem Journal und kopple diesen Computer.

linux.journalmark.pairingcancelledcurrentpairingstays = Kopplung abgebrochen. deine aktuelle Kopplung bleibt in Verwendung.

linux.journalmark.pairingcancelledgetfreshpairing = Kopplung abgebrochen. hol einen neuen Kopplungslink aus deinem Journal und kopple erneut, wenn du soweit bist.

linux.journalmark.pastepairinglinkfromjournal = füge den Kopplungslink aus deinem Journal ein und drücke dann Enter:{ $ws1 }

linux.journalmark.setupfailedpairinglinkwas = Ersteinrichtung fehlgeschlagen: der Kopplungslink war nicht gültig.

linux.journalmark.yousaidthismarkdoesn = du hast gesagt, diese mark stimmt nicht mit der überein, die dein Journal zeigt, deshalb ist dieser Computer nicht gekoppelt. du hast vielleicht den falschen Link eingefügt, oder etwas stimmt nicht. hol einen neuen Kopplungslink aus deinem Journal und kopple erneut, oder schreib an { $literal1 }, und wir helfen.

linux.metadata.autostartcomment = die solstone-App nimmt entgegen, was du mit ihr teilst, und alles davon geht in dein Journal

linux.metadata.autostartname = solstone-App

linux.metadata.gnomedescription = Helfer für Desktop-Aktivität für { $command }

linux.metadata.gnomename = solstone-Helfer für Aktivität

linux.metadata.kwindescription = KWin-Helfer für Aktivität für { $command }

linux.metadata.kwinname = solstone-Helfer für Aktivität

linux.metadata.name = solstone

linux.metadata.packagedescription = die solstone-App für Linux. sie nimmt den Bildschirm und den Ton entgegen, den du mit ihr teilst, und alles davon geht in dein Journal.

linux.metadata.packagesummary = die solstone-App für Linux: was du teilst, geht in dein Journal

linux.metadata.service = solstone-App für Linux

linux.notification.copied = kopiert

linux.notification.screenelementsmissing = Bildschirm-Annahme kann nicht starten: { $literal1 } fehlt { $elements }. installiere { $literal1 } { $literal2 }, die good- und base-Plugins, mit deinem Paketmanager und wähle dann auf der Bildschirmseite erneut versuchen.

linux.notification.screenelementsmissingx11 = Bildschirm-Annahme kann nicht starten: { $literal1 } fehlt { $elements }. installiere die good- und base-Plugins von { $literal1 } mit deinem Paketmanager und wähle dann auf der Bildschirmseite erneut versuchen.

linux.pair.cancel = abbrechen

linux.pair.continue = fortfahren

linux.pair.onemorestepcheckyourjournalsmarkyourjournal = noch ein Schritt: prüfe die mark deines Journals.{ $ws1 }{ $ws2 }mark deines Journals: { $spoken }{ $ws1 }{ $ws1 }dein Journal zeigt dieselbe mark in seiner Netzwerk-App. sie sollte genau übereinstimmen.{ $ws1 }stimmt das mit deinem Journal überein? gib { $literal3 } oder { $literal4 } ein:{ $ws1 }

linux.pair.onemorestepcheckyourjournalsmarkyourjournal2 = noch ein Schritt: prüfe die mark deines Journals.{ $ws1 }{ $ws2 }mark deines Journals: gerade nicht verfügbar{ $ws1 }{ $ws1 }das konnte nicht überprüft werden.{ $ws1 }dieser Computer konnte die mark deines Journals nicht erkennen, deshalb gibt es nichts zum Vergleichen. { $literal3 } nur, wenn du sicher bist, dass der Link aus deinem Journal kommt.{ $ws1 }gib { $literal3 } ein, um trotzdem zu koppeln, oder { $literal4 }, um aufzuhören:{ $ws1 }

linux.pair.pairingcancellednothingthiscomputerhaskeptwen = Kopplung abgebrochen. nichts, was dieser Computer behalten hat, ist über diesen Link in dieses Journal gegangen. hol einen neuen Kopplungslink aus deinem Journal und führe die Ersteinrichtung erneut aus, wenn du soweit bist.

linux.pair.themarkwordsyougavedontmatchthejournalsma = die Wörter der mark, die du angegeben hast, stimmen nicht mit der mark des Journals überein, deshalb ist dieser Computer nicht gekoppelt, und nichts, was dieser Computer behalten hat, ist über diesen Link in dieses Journal gegangen. prüfe die Wörter, hol einen neuen Kopplungslink aus deinem Journal und führe die Ersteinrichtung erneut aus.

linux.pair.thiscomputercouldntworkoutthejournalsmarks = dieser Computer konnte die mark des Journals nicht erkennen, deshalb können die Wörter, die du angegeben hast, nicht verglichen werden. dieser Computer ist nicht gekoppelt, und nichts, was dieser Computer behalten hat, ist über diesen Link in dieses Journal gegangen. hol einen neuen Kopplungslink aus deinem Journal und führe die Ersteinrichtung in einem Terminal aus, um selbst zu entscheiden.

linux.pair.thiscomputercouldntworkoutthejournalsmarks2 = dieser Computer konnte die mark des Journals nicht erkennen, deshalb können die Wörter, die du angegeben hast, nicht verglichen werden. warte auf deine Bestätigung der mark deines Journals. nichts, was wartet, geht in dein Journal, bevor du es tust. führe { $literal1 } in einem Terminal aus, um selbst zu entscheiden.

linux.pair.whenyourereadyrunsolstonelinuxconfirm = wenn du soweit bist, führe aus: { $literal1 }

linux.pair.yousaidthismarkdoesntmatchtheoneyourjourna = du hast gesagt, diese mark stimmt nicht mit der überein, die dein Journal zeigt, deshalb ist dieser Computer nicht gekoppelt, und nichts, was dieser Computer behalten hat, ist über diesen Link in dieses Journal gegangen. du hast vielleicht den falschen Link eingefügt, oder etwas stimmt nicht. hol einen neuen Kopplungslink aus deinem Journal und führe die Ersteinrichtung erneut aus, oder schreib an { $literal1 }, und wir helfen.

linux.panel.couldnotreachthedesktoptocheckwhatitneeds = der Desktop war nicht erreichbar, um zu prüfen, was er braucht

linux.panel.couldnotreachthesolstoneappx = die solstone-App war nicht erreichbar: { $reason }

linux.panel.couldnotreachthisdesktoptofindoutwhatitnee = dieser Desktop war nicht erreichbar, um herauszufinden, was er braucht.

linux.panel.donethepaneliconisinyourpanelnow = fertig. das Panel-Symbol ist jetzt in deinem Panel.

linux.panel.dontaskagain = nicht mehr fragen

linux.panel.erroractivityhelperassetsdirectorynotfound = Fehler: Verzeichnis der Helfer für Aktivität nicht gefunden

linux.panel.errorfailedtocopygnomeactivityhelperx = Fehler: { $literal1 }-Helfer für Aktivität konnte nicht kopiert werden: { $err }

linux.panel.errorfailedtocopykwinactivityhelperx = Fehler: { $literal1 }-Helfer für Aktivität konnte nicht kopiert werden: { $err }

linux.panel.errornoactivityhelperassetsfoundinx = Fehler: keine Helfer für Aktivität in { $source } gefunden

linux.panel.gnomegaveananswerwedonotknowx = { $literal1 } hat eine Antwort gegeben, die wir nicht kennen: { $other }

linux.panel.gnomeinstalledtheextensionbutcouldnotturnit = { $literal1 } hat die Erweiterung installiert, konnte sie aber nicht einschalten

linux.panel.gnomeissetnottoloadextensionssothepanelico = { $literal1 } ist so eingestellt, dass es keine Erweiterungen lädt, deshalb kann das Panel-Symbol auf diesem Weg nicht zurückkommen

linux.panel.gnomeneedsoneextensionbeforethepaneliconcan = { $literal1 } braucht eine Erweiterung, bevor das Panel-Symbol erscheinen kann. dort liegen Pausieren und Fortsetzen.

linux.panel.gnometooktheextensionbutthepaneliconhasnot = { $literal1 } hat die Erweiterung genommen, aber das Panel-Symbol ist nicht erschienen. führe { $literal2 } doctor aus, um zu sehen, wo es geblieben ist.

linux.panel.gnometooktheextensionbutthepaneliconhasnot2 = { $literal1 } hat die Erweiterung genommen, aber das Panel-Symbol ist noch nicht erschienen. warte einen Moment und führe dann aus: { $literal2 } doctor

linux.panel.gnomewillaskyoutoconfirmacceptittoaddthep = { $literal1 } fragt dich nach einer Bestätigung. nimm sie an, um die Panel-Symbol-Erweiterung hinzuzufügen.

linux.panel.gnomewouldnotturntheextensionon = { $literal1 } hat die Erweiterung nicht eingeschaltet

linux.panel.installedactivityhelperassetstouserdatadirect = Helfer für Aktivität in die privaten Datenverzeichnisse installiert.

linux.panel.logoutandbackinthenrunthisagain = melde dich ab und wieder an und führe das dann erneut aus.

linux.panel.nochangeyoucanrunthisagainanytime = keine Änderung. du kannst das jederzeit erneut ausführen.

linux.panel.noinstalledappindicatorextensiontoturnon = keine installierte appindicator-Erweiterung zum Einschalten

linux.panel.noteyoumustexplicitlyenabletheextensionorscr = Hinweis: du musst die Erweiterung oder das Skript in deiner Desktop-Sitzung ausdrücklich einschalten.

linux.panel.nothingherecansetupapanelicononthisdesktop = hier kann nichts ein Panel-Symbol auf diesem Desktop einrichten.

linux.panel.nothingtodo = nichts zu tun.

linux.panel.nothingtosetuphere = hier ist nichts einzurichten.

linux.panel.notnow = nicht jetzt

linux.panel.notrunning = die solstone-App läuft nicht, deshalb gibt es nichts zu pausieren oder fortzusetzen.

linux.panel.pausedforxminutesrunsolstonelinuxresumetoe = pausiert für { $minutes } Minute(n). führe { $literal1 } resume aus, um es früher zu beenden.

linux.panel.pausedrunsolstonelinuxresumewhenyouwantitba = pausiert. führe { $literal1 } resume aus, wenn du es zurück willst.

linux.panel.setitup = einrichten

linux.panel.solstoneapp = solstone-App

linux.panel.thedownloadedextensioncouldnotbeunpacked = die heruntergeladene Erweiterung konnte nicht entpackt werden

linux.panel.theextensioncouldnotbedownloadedcheckthenetw = die Erweiterung konnte nicht heruntergeladen werden; prüfe die Netzwerkverbindung

linux.panel.thegnomeextensionisalreadyonifthepanelicon = die Erweiterung { $literal1 } ist schon an. wenn das Panel-Symbol noch fehlt, melde dich ab und wieder an.

linux.panel.thegnomeextensionisinstalledbutturnedoffturn = die Erweiterung { $literal1 } ist installiert, aber aus. sie wird jetzt eingeschaltet.

linux.panel.thepaneliconextensionisinstalledbutgnomehas = die Panel-Symbol-Erweiterung ist installiert, aber { $literal1 } hat sie noch nicht übernommen.

linux.panel.thepaneliconextensionisinstalledlogoutandba = die Panel-Symbol-Erweiterung ist installiert. melde dich ab und wieder an und führe dann { $literal1 } erneut aus.

linux.panel.thepaneliconisalreadyavailableonthisdesktop = das Panel-Symbol ist auf diesem Desktop schon verfügbar.

linux.panel.thepaneliconisnotsetupx = das Panel-Symbol ist nicht eingerichtet: { $reason }

linux.panel.thepaneliconisreadypauseandresumeareinthe = das Panel-Symbol ist bereit. Pausieren und Fortsetzen sind jetzt im Panel.

linux.panel.thesolstoneappisnotrunningsothereisnothing = die solstone-App läuft nicht, deshalb gibt es nichts zu pausieren oder fortzusetzen.

linux.panel.thisdesktopdoesnotallowextensionstobeinstall = dieser Desktop erlaubt nicht, Erweiterungen zu installieren

linux.panelicon.resumed = fortgesetzt.

linux.screenshots.bringcount = { $count ->
    [one] 1 Bildschirmfoto hereinholen
   *[other] { $formatted } Bildschirmfotos hereinholen
}

linux.screenshots.refusedcount = { $count ->
    [one] 1 Bildschirmfoto kann nicht hereinkommen, deshalb bleibt es im Ordner.
   *[other] { $formatted } Bildschirmfotos können nicht hereinkommen, deshalb bleiben sie im Ordner.
}

linux.screenshots.total = { $count ->
    [one] 1 Bildschirmfoto
   *[other] { $formatted } Bildschirmfotos
}

linux.screenshots.unreadablecount = { $count ->
    [one] 1 Bildschirmfoto konnte nicht gelesen werden.
   *[other] { $formatted } Bildschirmfotos konnten nicht gelesen werden.
}

linux.service.currentexecutablehasnoparentdirectory = aktuelle Programmdatei ohne übergeordnetes Verzeichnis

linux.service.errorremovingxx = Fehler beim Entfernen von { $value1 }: { $error }

linux.service.errorwritingservicefilesx = Fehler beim Schreiben der Dienstdateien: { $error }

linux.service.homeisnotsetcannotlocateuserservicefiles = HOME ist nicht gesetzt; die Dienstdateien für diese Anmeldung sind nicht zu finden

linux.service.invalidservicepid = ungültige Dienst-PID

linux.service.removed = Entfernt { $value1 }

linux.service.servicepidunavailable = Dienst-PID nicht verfügbar

linux.service.servicerestartfailedx = Dienstneustart fehlgeschlagen: { $error }

linux.service.systemctlcouldnotrestartsolstonelinuxservicex = systemctl konnte { $literal1 }.service nicht neu starten:{ $ws2 }{ $value1 }

linux.service.systemctlnotfound = systemctl nicht gefunden

linux.service.systemctlstopfailed = systemctl stop fehlgeschlagen

linux.service.warningcouldnotremoveerror = Warnung: { $value1 } konnte nicht entfernt werden: { $error }

linux.service.warningcouldnotremoveeverybrowserregistration = Warnung: nicht jede Browser-Registrierung konnte entfernt werden: { $error }

linux.service.warningcouldnotwriteeverybrowserregistrationx = Warnung: nicht jede Browser-Registrierung konnte geschrieben werden: { $error }

linux.service.warningsystemctlenablenowfailedrunsystemctlus = Warnung: systemctl enable --now ist fehlgeschlagen; führe '{ $literal1 }' aus, um den Fehler anzusehen

linux.service.warningsystemctlenablenowfailedxrunsystemctl = Warnung: systemctl enable --now ist fehlgeschlagen: { $error }; führe '{ $literal1 }' aus, sobald systemd verfügbar ist

linux.service.warningsystemctlstartfailedrunsystemctluserst = Warnung: systemctl start ist fehlgeschlagen; führe '{ $literal1 }' aus, um den Fehler anzusehen

linux.service.warningsystemctlstartfailedxrunsystemctluser = Warnung: systemctl start ist fehlgeschlagen: { $error }; führe '{ $literal1 }' aus, sobald systemd verfügbar ist

linux.service.warningsystemctlxfailedrunsystemctluserxtoi = Warnung: systemctl { $operation } ist fehlgeschlagen; führe '{ $literal1 } { $operation }' aus, um den Fehler anzusehen

linux.service.warningsystemctlxfailedxrunsystemctluserxaf = Warnung: systemctl { $operation } ist fehlgeschlagen: { $error }; führe '{ $literal1 } { $operation }' aus, sobald systemd verfügbar ist

linux.service.wrotex = Geschrieben { $value1 }

linux.shared.cli.versionlastknown = { $value1 } (zuletzt bekannt)

linux.shared.copy.connected = verbunden

linux.shared.copy.status = Status

linux.shared.copy.yourjournalshowsthissamemarkinitsnetworkapp = dein Journal zeigt dieselbe mark in seiner Netzwerk-App. sie sollte genau übereinstimmen.

linux.shared.health.unknown = unbekannt

linux.shared.pair.confirmcantaskyouaboutyourjournalsmarkhere = confirm kann dich hier nicht nach der mark deines Journals fragen. führe es in einem Terminal aus oder gib die zwei Wörter der mark mit --mark "word word" an.

linux.shared.pair.couldntverify = das konnte nicht überprüft werden.

linux.shared.pair.doesthismatchyourjournaltypeyesorno = stimmt das mit deinem Journal überein? gib { $literal1 } oder { $literal2 } ein:{ $ws3 }

linux.shared.pair.givethetwowordsofyourjournalsmarklikemark = gib die zwei Wörter der mark deines Journals an, wie { $literal1 }.

linux.shared.pair.notpaired = nicht gekoppelt.

linux.shared.pair.setupcantaskyouaboutyourjournalsmarkherer = setup kann dich hier nicht nach der mark deines Journals fragen. führe es in einem Terminal aus oder gib die zwei Wörter der mark mit --mark "word word" an.

linux.shared.pair.thetwowordsofthemarkyourjournalsnetworkapp = die zwei Wörter der mark, die die Netzwerk-App deines Journals zeigt. nötig, wenn kein Terminal da ist, das dich fragen kann.

linux.shared.pair.typecontinuetopairanywayorcanceltostop = gib { $literal1 } ein, um trotzdem zu koppeln, oder { $literal2 }, um aufzuhören:{ $ws3 }

linux.shared.pair.waitingforyoutoconfirmyourjournalsmarknothi = warte auf deine Bestätigung der mark deines Journals. nichts, was wartet, geht in dein Journal, bevor du es tust.

linux.shared.pair.yourjournalsmarkisalreadyconfirmednothingto = die mark deines Journals ist schon bestätigt. nichts zu tun.

linux.shared.tray.offline = offline

linux.sharedsources.also = auch auf

linux.sharedsources.cli = Obsidian: { $state }

linux.sharedsources.many = von anderen Geräten: { $count }

linux.sharedsources.one = von einem anderen Gerät

linux.sharedsources.pair = { $first } · { $second }

linux.shell.activitystillon = Aktivität ist noch an.

linux.shell.afternoon = guten Nachmittag

linux.shell.amountsince = { $total }, seit { $date }

linux.shell.attention = braucht Aufmerksamkeit

linux.shell.browserunavailable = Browserseiten können gerade nicht starten.

linux.shell.checking = wird geprüft…

linux.shell.evening = guten Abend

linux.shell.hourcount = { $count ->
    [one] { $display } Stunde
   *[other] { $display } Stunden
}

linux.shell.hoursago = { $count ->
    [one] vor { $display } Stunde
   *[other] vor { $display } Stunden
}

linux.shell.intakeunreadable = die Aufzeichnung, wie viel entgegengenommen wurde, konnte nicht gelesen werden.

linux.shell.itemcount = { $count ->
    [one] { $display } Element
   *[other] { $display } Elemente
}

linux.shell.lessthanminute = weniger als eine Minute

linux.shell.lessthanminuteleft = pausiert, weniger als eine Minute übrig

linux.shell.minutecount = { $count ->
    [one] { $display } Minute
   *[other] { $display } Minuten
}

linux.shell.minutesago = vor { $minutes } min

linux.shell.month.april = April

linux.shell.month.august = August

linux.shell.month.december = Dezember

linux.shell.month.february = Februar

linux.shell.month.january = Januar

linux.shell.month.july = Juli

linux.shell.month.june = Juni

linux.shell.month.march = März

linux.shell.month.may = Mai

linux.shell.month.november = November

linux.shell.month.october = Oktober

linux.shell.month.september = September

linux.shell.monthday = { $day }. { $month }

linux.shell.monthdayyear = { $day }. { $month } { $year }

linux.shell.morning = guten Morgen

linux.shell.notecount = { $count ->
    [one] { $display } Notiz
   *[other] { $display } Notizen
}

linux.shell.pagecount = { $count ->
    [one] { $display } Seite
   *[other] { $display } Seiten
}

linux.shell.pauseleft = pausiert, noch { $minutes } min

linux.shell.readytosetup = bereit zum Einrichten

linux.shell.screenmissingplugin = ein { $literal1 }-Plugin fehlt.

linux.shell.screensharingnotallowed = Bildschirmfreigabe war nicht erlaubt.

linux.shell.screenshotcount = { $count ->
    [one] { $display } Bildschirmfoto
   *[other] { $display } Bildschirmfotos
}

linux.shell.screenstartfailed = Bildschirm-Annahme konnte nicht starten.

linux.shell.settingup = wird eingerichtet

linux.shell.terminalunavailable = Terminal-Sitzungen können gerade nicht starten.

linux.shell.tileneedsattention = { $source } braucht Aufmerksamkeit

linux.shell.untilresume = pausiert, bis du fortsetzt

linux.shell.withaudio = mit Ton

linux.shell.withaudioactivity = mit Ton und Aktivität

linux.shell.yesterday = gestern

linux.state.stoppedbyanerrorrestarting = durch einen Fehler gestoppt, startet neu

linux.state.terminalisoff = Terminal ist aus

linux.state.tmuxnotfound = { $literal1 } nicht gefunden

linux.state.waitingsolstonefortmuxisstillinstalled = wartet: solstone für { $literal1 } ist noch installiert. schalte das Terminal in der App ein oder führe `{ $literal2 } terminal on` aus, um es herüberzunehmen

linux.status.browsernobrowserconnected = Browser: kein Browser verbunden

linux.status.browserthesolstoneappisnotrunning = Browser: die solstone-App läuft nicht

linux.status.heldxsegmentsnotsent = Zurückgehalten: { $value1 } Segment(e) nicht gesendet

linux.status.heldxsegmentsnotsentoldestxd = Zurückgehalten: { $value1 } Segment(e) nicht gesendet, ältestes { $value2 } d

linux.status.pausedsononewpagesaretaken = { $ws1 }pausiert, deshalb werden keine neuen Seiten entgegengenommen

linux.status.theroomforbrowserpagesonthis = { $ws1 }der Platz für Browserseiten auf diesem Computer ist voll, deshalb werden keine neuen Seiten entgegengenommen, bis welche in dein Journal gehen

linux.sync.checkingjournal = prüft das Journal...

linux.sync.checkingx = prüft { $day }...

linux.sync.probefailednextinxs = { $count ->
    [one] Prüfung fehlgeschlagen, nächste in { $count } Sekunde
   *[other] Prüfung fehlgeschlagen, nächste in { $count } Sekunden
}

linux.sync.probingjournal = Journal-Prüfung...

linux.sync.syncing = Abgleich...

linux.sync.uploadingx = lädt { $segment-key } hoch

linux.sync.xsuntilprobe = { $count ->
    [one] { $count } Sekunde bis zur Prüfung
   *[other] { $count } Sekunden bis zur Prüfung
}

linux.synchealth.ifyourecentlyresetjournal = wenn du dein Journal kürzlich zurückgesetzt hast, kopple dieses Gerät erneut.

linux.synchealth.thisislesscertainthan = das ist weniger gewiss als eine direkte Verbindung.

linux.terminal.control.bringold = holt herüber, was wartet

linux.terminal.control.confirmationsolstoneappisnt = { $confirmation } die solstone-App läuft nicht; das gilt, sobald sie startet.

linux.terminal.control.couldnotmovesolstonetmux = solstone für { $literal1 } konnte nicht hereingeholt werden: { $error }

linux.terminal.control.couldnotreachsolstoneapp = die solstone-App war nicht erreichbar: { $error }

linux.terminal.control.couldnotrecordmoveyet = das Verschieben konnte noch nicht festgehalten werden: { $error }

linux.terminal.control.couldnotsavethaterror = das konnte nicht gespeichert werden: { $error }

linux.terminal.control.couldntuseproblem = { $ws1 }{ $value1 } konnte nicht genutzt werden: { $problem }

linux.terminal.control.ignoredsettingkey = { $ws1 }Einstellung übergangen: { $key }

linux.terminal.control.isoff = Terminal ist aus.

linux.terminal.control.ison = Terminal ist an.

linux.terminal.control.morethanoneoldpairing = { $ws1 }mehr als eine alte Kopplung: { $value1 }

linux.terminal.control.moving = holt solstone für tmux herein.

linux.terminal.control.movingsolstonetmuxneedsok = das Hereinholen von solstone für { $literal1 } braucht dein OK. führe das in einem Terminal aus oder füge --yes zu `terminal adopt-{ $literal1 }` hinzu.

linux.terminal.control.movingsolstonetmuxphase = { $ws1 }holt solstone für { $literal2 } herein: { $phase }

linux.terminal.control.newterminalhistoryfromhere = { $ws1 }neue Terminal-Historie ab hier, und was schon in deinem Journal ist, bleibt ({ $reason })

linux.terminal.control.nosolstonetmuxmove = kein solstone für { $literal1 } zum Hereinholen.

linux.terminal.control.oldsegmentsheldback = { $ws1 }{ $value1 } alte Segment(e) zurückgehalten

linux.terminal.control.problem = { $ws1 }Problem: { $value1 }

linux.terminal.control.recordmovingsolstonetmuxcan = die Aufzeichnung des Verschiebens von solstone für { $literal1 } kann nicht gelesen werden: { $value1 }. bitte wende dich an den Support, und lösche sie nicht.

linux.terminal.control.screenaudiorestartappafter = Bildschirm und Ton: starte die App neu, nachdem du dich hier an deinem Desktop angemeldet hast

linux.terminal.control.screenisoff = Bildschirm, Audio und Desktop-Aktivität sind aus.

linux.terminal.control.screenison = Bildschirm, Audio und Desktop-Aktivität sind an.

linux.terminal.control.screenoffstatus = Bildschirm, Audio und Desktop-Aktivität: aus

linux.terminal.control.segmentsfromsolstonetmux = { $ws1 }{ $value1 } Segment(e) von solstone für { $literal2 } warten auf den Upload

linux.terminal.control.segmentsheld = { $ws1 }{ $value1 } Segment(e) zurückgehalten

linux.terminal.control.segmentswaitingupload = { $ws1 }{ $value1 } Segment(e) warten auf den Upload

linux.terminal.control.solstoneappdidnotanswer = die solstone-App hat nicht rechtzeitig geantwortet; versuch es gleich noch einmal

linux.terminal.control.solstoneappdidnotanswer2 = die solstone-App hat nicht geantwortet, deshalb hat sich nichts geändert; versuch es gleich noch einmal.

linux.terminal.control.solstoneappisstartingtry = die solstone-App startet; versuch es gleich noch einmal.

linux.terminal.control.solstoneappisstillmoving = die solstone-App holt solstone für { $literal1 } noch herein; es wird von selbst fertig.

linux.terminal.control.solstoneappneedsrestartingbefore = die solstone-App muss neu starten, bevor sie solstone für { $literal1 } hereinholen kann.

linux.terminal.control.solstonetmuxhasalreadymoved = solstone für { $literal1 } ist schon hereingeholt.

linux.terminal.control.solstonetmuxhasmoved = solstone für { $literal1 } ist hereingeholt.

linux.terminal.control.solstonetmuxhasmovedsolstone = solstone für { $literal1 } ist hereingeholt. die solstone-App macht den Rest, sobald sie startet.

linux.terminal.control.solstonetmuxisstillinstalled = solstone für { $literal1 } ist auf diesem Computer noch installiert, deshalb bleibt das Terminal vorerst aus

linux.terminal.control.stopold = beendet die alte App

linux.terminal.control.thismovessolstonetmuxinto = das verschiebt solstone für { $literal1 } in diese App, und es endet endgültig. was noch wartet, geht über diese App in dein Journal, und was nicht gehen kann, bleibt auf diesem Computer. wenn diese App noch nicht gekoppelt ist, koppelt sie mit dem Journal, das solstone für { $literal1 } nutzt. jetzt verschieben? [y/N]

linux.terminal.control.tmux = { $ws1 }{ $literal2 }: { $value1 }

linux.terminal.control.unfinishedsegmentswaitingterminal = { $ws1 }{ $value1 } unfertige Segment(e) warten, bis das Terminal wieder läuft

linux.terminal.control.waitinganswer = { $ws1 }wartet: { $answer }

linux.terminal.malformedcommand = fehlerhafter Befehl

linux.terminal.state.stoppedbyerrorrestartingerror = durch einen Fehler gestoppt, startet neu: { $error }

linux.terminal.takeover.couldnotremoveerror = konnte { $value1 } nicht entfernen: { $error }

linux.terminal.takeover.isnotsolstonetmuxservice = { $value1 } ist nicht der Dienst von solstone für { $literal1 }, deshalb wurde nichts geändert.

linux.terminal.takeoverrunner.couldnotsaveterminalsettings = die Terminal-Einstellungen konnten nicht gespeichert werden: { $error }

linux.terminal.takeoverrunner.couldnotturnterminalerror = das Terminal konnte nicht eingeschaltet werden: { $error }

linux.terminal.warning = Warnung: { $detail }

linux.tray.aboutsolstone = über solstone

linux.tray.cachexmb = Cache: { $value1 } MB

linux.tray.cantcheckwhichsessionisinfront = kann nicht prüfen, welche Sitzung vorn ist

linux.tray.cantreachyourdesktop = dein Desktop ist nicht erreichbar

linux.tray.connectedallcaughtup = verbunden · alles aufgeholt

linux.tray.idlescreeninactive = Leerlauf (Bildschirm inaktiv)

linux.tray.n15minutes = 15 Minuten

linux.tray.n1hour = 1 Stunde

linux.tray.n30minutes = 30 Minuten

linux.tray.notpaired = nicht gekoppelt

linux.tray.notrunning = läuft nicht

linux.tray.offlinewaiting = offline · wartend: { $count }

linux.tray.openjournal = Journal öffnen

linux.tray.opensettings = Einstellungen öffnen

linux.tray.pausedxmremaining = { $count ->
    [one] pausiert (noch { $count } Minute)
   *[other] pausiert (noch { $count } Minuten)
}

linux.tray.quitsolstone = solstone beenden

linux.tray.resume = fortsetzen

linux.tray.resumexmremaining = { $count ->
    [one] fortsetzen (noch { $count } Minute)
   *[other] fortsetzen (noch { $count } Minuten)
}

linux.tray.screenintakecantstart = Bildschirm-Annahme kann nicht starten

linux.tray.screenintakecouldntstart = Bildschirm-Annahme konnte nicht starten

linux.tray.screenneedsattention = Bildschirm braucht Aufmerksamkeit

linux.tray.screensharingnotallowed = Bildschirmfreigabe war nicht erlaubt

linux.tray.tooltip = { $status }{ $break }{ $health }

linux.tray.untiliresume = bis ich fortsetze

linux.tray.waitingforscreenintake = wartet auf die Bildschirm-Annahme

linux.tray.waitingforyourdesktop = wartet auf deinen Desktop

linux.tray.waitingnoneofyoursessionsisinfront = wartet (keine deiner Sitzungen ist vorn)

linux.tray.waitingyoursessionisntinfront = wartet (deine Sitzung ist nicht vorn)

linux.ui.terminalterminalstatus = Terminal: { $terminal-status }

linux.window.addmore = mehr hinzufügen

linux.window.allcaughtup = alles auf dem neuesten Stand

linux.window.brightharbor = bright · harbor

linux.window.browsers = Browser

linux.window.browsersbreath = auf den Websites, die du erlaubst, nimmt die solstone-Erweiterung den Text und das grobe Layout der Seite entgegen, und alles davon geht in dein Journal.

linux.window.browsersgap = Seiten kommen von der solstone-Erweiterung. hier gibt es keinen Schalter dafür.

linux.window.cancel = abbrechen

linux.window.checkingyourjournal = prüft dein Journal.

linux.window.choosefiles = Dateien wählen…

linux.window.choosenewdownloadname = wähle einen neuen Namen, um die vorhandene Datei zu behalten

linux.window.close = schließen

linux.window.connectajournal = Journal verbinden

linux.window.connected = verbunden

linux.window.connection = Verbindung

linux.window.everythinginyourjournal = alles ist in deinem Journal

linux.window.files = Dateien

linux.window.foregroundtitleurlidle = { $route }; Vordergrund { $foreground }; Titel { $title }; Adresse { $url }; Leerlauf { $idle }

linux.window.goesalongwithit = geht damit einher

linux.window.help = Hilfe

linux.window.home = ‹ Start

linux.window.home.vcb176487 = Start

linux.window.import = Import

linux.window.keptonthiscomputer = auf diesem Computer behalten

linux.window.kilobytes = { $size } KB

linux.window.lastreached = zuletzt erreicht

linux.window.lasttakenin = zuletzt entgegengenommen

linux.window.leftout = ausgelassen

linux.window.megabytes = { $size } MB

linux.window.nothingyet = noch nichts

linux.window.notpaired = nicht gekoppelt

linux.window.obsidian = { $literal1 }

linux.window.offsub = schalte es jederzeit ein.

linux.window.ok = ok

linux.window.onthisdevice = auf diesem Gerät

linux.window.pause = pausieren ▾

linux.window.recent = neuere

linux.window.remove = entfernen

linux.window.remove.vf022f0fa = { $value1 } entfernen?

linux.window.resume = fortsetzen

linux.window.screen = Bildschirm

linux.window.screenaudioandactivitystillon = Ton und Aktivität sind noch an.

linux.window.screenaudiostillon = Ton ist noch an.

linux.window.screenbreath = solange das an ist, nimmt die solstone-App entgegen, was auf deinem Bildschirm ist, den Ton und welche Apps und Fenster du nutzt, und alles davon geht in dein Journal.

linux.window.screenloginunavailable = kann nicht prüfen, welche Sitzung vorn ist.

linux.window.screenmissingelements = Bildschirm-Annahme kann nicht starten: { $literal1 } fehlt { $elements }. installiere { $literal1 } { $literal2 }, die good- und base-Plugins, mit deinem Paketmanager und wähle dann erneut versuchen.

linux.window.screenmissingelementsx11 = Bildschirm-Annahme kann nicht starten: { $literal1 } fehlt { $elements }. installiere die good- und base-Plugins von { $literal1 } mit deinem Paketmanager und wähle dann erneut versuchen.

linux.window.screenretrying = die Bildschirm-Annahme ist beim ersten Versuch nicht gestartet. versucht es gleich noch einmal.

linux.window.screensharingnotallowed = Bildschirmfreigabe war nicht erlaubt. wähle erneut versuchen, um deinen Bildschirm zu teilen.

linux.window.screensound = der Ton umfasst dein Mikrofon und was dieser Computer abspielt. wenn kein Mikrofon da ist, kommt beides nicht herein.

linux.window.screenstartfailed = Bildschirm-Annahme konnte nicht starten.

linux.window.screenunreachable = dein Desktop ist nicht erreichbar.

linux.window.screenwaiting = wartet darauf, den Bildschirm entgegenzunehmen.

linux.window.sidebar = Seitenleiste

linux.window.sofar = bisher

linux.window.somethingyouwereaddingdidntfinish = etwas, das du hinzugefügt hast, wurde nicht fertig gespeichert. füge es noch einmal hinzu.

linux.window.sources = Quellen

linux.window.status = Status

linux.window.status.vaff1290e = ‹ Status

linux.window.technicaldetails = technische Details

linux.window.terminal = Terminal

linux.window.terminalbreath = die solstone-App nimmt deine { $literal1 }-Sitzungen entgegen, und alles davon geht in dein Journal. braucht { $literal1 } auf diesem Computer und funktioniert in jeder Terminal-Sitzung, auch wenn du per { $literal2 } auf diesen Computer kommst.

linux.window.thecopyisalreadygonesothis = die Kopie ist schon weg, also wird sie nur von hier entfernt.

linux.window.thiscomputer = dieser Computer

linux.window.thisremovesthecopyonthisdevice = das entfernt die Kopie auf diesem Gerät. dein Journal kann die Kopie, die es schon hat, weiterhin hereinholen.

linux.window.thisremovesthecopyonthisdevice.v558a0c4f = das entfernt die Kopie auf diesem Gerät, also erreicht sie dein Journal nicht.

linux.window.trying = wird versucht…

linux.window.trynow = jetzt versuchen

linux.window.updates = Aktualisierungen

linux.window.yourjournal = dein Journal

linux.window.yourjournal.vc43fd4d5 = dein · Journal
