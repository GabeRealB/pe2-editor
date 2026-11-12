# Models

Characters, weapons and objects are made of several kinds of asset:

| Kind | What it is |
|---|---|
| **Mesh** | A shape. |
| **Material** | How a surface looks: its texture and how it is lit. |
| **Skeleton** | The joints a model moves by. |
| **Model** | Meshes on a skeleton, with materials. |
| **Prefab** | Models put together, such as a character holding a weapon. |
| **Clip** | One animation. |
| **Animation bank**, **Animation schema** | A model's animations together, and what each is for. |

## The 3D view

Models, meshes, skeletons and prefabs open in the **3D** view.

| To | Do |
|---|---|
| Orbit | Drag with the middle mouse button |
| Pan | `Shift` and drag with the middle button |
| Zoom | The mouse wheel, or `Ctrl` and drag |
| Look along an axis | Numpad `1`, `3`, `7`; numpad `5` switches perspective |
| Frame everything / the selection | `Home` / `.` |
| Select a part | Click it |
| Move, turn or scale a part | Drag its handles |

The toolbar switches between **Solid** and **Wireframe**, shows or hides the **Grid**, **Bones** and **Normals**,
switches to **Orthographic**, **Frame**s the view, and **Isolate**s the selected part. The paint menu colors the
model by its **Textures**, its **Shading**, or where its colors come from.

Clips play in the same view: **Play**/**Pause**, **Frame back**, **Frame on** and **Loop**; `Space` plays, the arrow
keys step.

## Textures

The **UV** view shows each texture a model uses, with the model's faces traced on it. Click a face to select its
part.

The Inspector's **Materials** view lists the model's materials. Drop a material, texture or palette on one to
change it, or **Override the texture** for this model alone; **Revert to the model's** undoes that.

## Putting models together

The Contents pane shows a prefab's **Hierarchy**. Drop a model on a joint to attach it there; drag parts to move
them; **Detach** or **Remove the part** to take one off, **Revert to the skeleton** to start over.
