// lan-mouse-ui is the Go frontend of lan-mouse. It connects to the
// running service, or starts one with `lan-mouse daemon` and stops it
// again on exit (design D4). The interface is drawn by mygo's native
// UI path: no webview, no HTML.
package main

import (
	"bytes"
	"image"
	"image/color"
	"image/png"
	"log"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/ui"
)

// trayIcon renders a small filled square as a template PNG for the menu
// bar item, standing in until the app ships a real icon.
func trayIcon() []byte {
	img := image.NewRGBA(image.Rect(0, 0, 16, 16))
	for y := 4; y < 12; y++ {
		for x := 4; x < 12; x++ {
			img.Set(x, y, color.Black)
		}
	}
	var buf bytes.Buffer
	if err := png.Encode(&buf, img); err != nil {
		log.Printf("tray icon: %v", err)
		return nil
	}
	return buf.Bytes()
}

func main() {
	a := newApp()
	done := make(chan struct{})

	mygo.App.WhenReady(func() {
		window := mygo.NewWindow(mygo.WindowOptions{
			Title:   "Lan Mouse",
			Width:   720,
			Height:  640,
			Content: ui.View(a.view),
		})
		a.setWindow(window)

		// closing the window exits the frontend; a daemon this frontend
		// started is stopped in main after App.Run returns
		window.OnClosed(func() { mygo.App.Quit() })

		menu := mygo.NewMenu([]*mygo.MenuItem{
			{
				ID:    "show",
				Label: "Show window",
				Click: func(item *mygo.MenuItem, win *mygo.Window) {
					window.Show()
					window.Focus()
				},
			},
			{
				ID:    "quit",
				Label: "Quit",
				Click: func(item *mygo.MenuItem, win *mygo.Window) {
					mygo.App.Quit()
				},
			},
		})
		if _, err := mygo.NewTray(mygo.TrayOptions{
			ID:             "lan-mouse",
			Icon:           trayIcon(),
			IconIsTemplate: true,
			Title:          "lan-mouse",
			ToolTip:        "Lan Mouse",
			Menu:           menu,
		}); err != nil {
			log.Printf("menu bar item unavailable: %v", err)
		}

		go a.runConnection(done)
	})

	err := mygo.App.Run()
	close(done)
	a.shutdown()
	if err != nil {
		log.Fatal(err)
	}
}
