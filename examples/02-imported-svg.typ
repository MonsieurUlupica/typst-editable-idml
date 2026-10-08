#set page(width: 180mm, height: 180mm, margin: 12mm)
#set text(font: "Libertinus Serif", size: 13pt)
= Gráficos SVG integrados
Dos instancias de un gráfico original con recorte, marcador y texto.

#grid(columns: (1fr, 1fr), gutter: 5mm,
  image("assets/diagram.svg", width: 100%),
  image("assets/diagram.svg", width: 100%),
)

Imagen PNG incrustada: #image("assets/pattern.png", width: 16mm)
