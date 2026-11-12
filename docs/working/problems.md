# Problems

The **Problems** pane lists what needs your attention, grouped by where it comes from. The status bar shows how
many there are; click it to open the pane. Click a problem to go to the asset it is about.

| Problem | What to do |
|---|---|
| **An override is out of date**: the version below changed in a way that conflicts with yours. | Make its layer active, then **Keep mine** or **Take below**. See [Layers and overrides](layers.md). |
| **A file was put in a layer's folder by hand.** | **Adopt** it into the project, or **Remove** it. |
| **A file the layer expects is missing** from its folder. | **Forget it**, or take the version below. |
| **A reference leads nowhere**: an asset refers to one that is gone. | Point the field at another asset, or remove it. |
| **An extension is not trusted, or could not be loaded**, so what it makes is missing. | See [Extensions](../extending/extensions.md). |
| **A build failed**, or an output is out of date. | The problem says why; fix the asset and build again. See [Building](building.md). |
| **A configuration's setting is not what it can be.** | Change it in **Settings ▸ Configurations**. See [Configurations](../making/configurations.md). |
| **A stack or graph does not add up**, such as a wire between things that do not fit. | Fix it in the Inspector's Stack tab or the graph editor. See [Stacks](../making/stacks.md). |

Problems disappear on their own once fixed.
