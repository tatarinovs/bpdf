import os, math
from PIL import Image, ImageDraw, ImageFont

def get_font(size, bold=True):
    try:
        font_name = "arialbd.ttf" if bold else "arial.ttf"
        return ImageFont.truetype(font_name, size)
    except OSError:
        try:
            return ImageFont.truetype("arial.ttf", size)
        except OSError:
            return ImageFont.load_default()

def draw_star(draw, cx, cy, r_outer, r_inner, fill):
    points = []
    for i in range(10):
        r = r_outer if i % 2 == 0 else r_inner
        angle = i * math.pi / 5 - math.pi / 2
        x = cx + r * math.cos(angle)
        y = cy + r * math.sin(angle)
        points.append((x, y))
    draw.polygon(points, fill=fill)

def draw_padlock(draw, keyhole, opened):
    # Bold white padlock filling the tile, legible at 16 px. An open lock
    # has its shackle lifted, with only the left leg still in the body.
    white = (255, 255, 255, 255)
    lift = 22 if opened else 0
    draw.arc([82, 34 - lift, 174, 126 - lift], start=180, end=360, fill=white, width=24)
    draw.line([(94, 80 - lift), (94, 124)], fill=white, width=24)
    right_end = 92 - lift if opened else 124
    draw.line([(162, 80 - lift), (162, right_end)], fill=white, width=24)
    draw.rounded_rectangle([54, 116, 202, 230], radius=20, fill=white)
    draw.ellipse([112, 142, 144, 174], fill=keyhole)
    draw.polygon([(118, 164), (138, 164), (144, 208), (112, 208)], fill=keyhole)

def create_icon_perfect(action):
    img = Image.new('RGBA', (256, 256), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)

    # Vibrant Colors
    c_blue = (0, 114, 227, 255)
    c_green = (40, 167, 69, 255)
    c_red = (220, 53, 69, 255)
    c_purple = (111, 66, 193, 255)
    c_orange = (253, 126, 20, 255)
    c_cyan = (0, 153, 204, 255)
    c_teal = (32, 201, 151, 255)
    c_dark = (52, 58, 64, 255)
    c_pink = (232, 62, 140, 255)
    c_gold = (255, 185, 0, 255)
    c_violet = (102, 16, 242, 255)
    white = (255, 255, 255, 255)

    color_map = {
        "merge": c_blue,
        "ocr": c_green,
        "strip": c_red,
        "split": c_purple,
        "extract": c_orange,
        "rotate": c_cyan,
        "text": c_teal,
        "optimize": c_dark,
        "images": c_pink,
        "stamp": c_gold,
        "convert": c_violet,
        "number": c_pink,
        "watermark": (70, 110, 160, 255),
        "ocr-inplace": (25, 135, 84, 255),
        "protect": (157, 30, 70, 255),
        "unlock": (20, 150, 120, 255),
        "bpdf": (18, 57, 184, 255)
    }
    bg_color = color_map.get(action, c_blue)

    # Base tile (full bleed square, no transparency)
    draw.rectangle([0, 0, 256, 256], fill=bg_color)
    if action == "merge":
        draw.polygon([(40, 36), (130, 36), (160, 66), (160, 176), (40, 176)], fill=(190, 215, 250, 255))
        draw.polygon([(90, 76), (180, 76), (210, 106), (210, 216), (90, 216)], fill=white)
        draw.ellipse([105, 105, 215, 215], fill=white, outline=c_blue, width=6)
        draw.line([(160, 126), (160, 194)], fill=c_blue, width=22)
        draw.line([(126, 160), (194, 160)], fill=c_blue, width=22)

    elif action == "ocr":
        draw.polygon([(45, 24), (165, 24), (205, 64), (205, 224), (45, 224)], fill=white)
        draw.rectangle([35, 114, 215, 146], fill=(0, 230, 120, 230))
        font = get_font(124, bold=True)
        draw.text((82, 58), "A", fill=(20, 30, 40, 255), font=font)

    elif action == "strip":
        draw.polygon([(45, 24), (165, 24), (205, 64), (205, 224), (45, 224)], fill=white)
        draw.line([(78, 74), (172, 168)], fill=c_red, width=30)
        draw.line([(172, 74), (78, 168)], fill=c_red, width=30)

    elif action == "split":
        # Two White PDF Pages separating + Gold Outward Split Arrows (◄ ►) & Dashed Line
        # Left Page
        draw.polygon([(32, 32), (102, 32), (116, 46), (116, 224), (32, 224)], fill=white)
        draw.polygon([(102, 32), (102, 46), (116, 46)], fill=(200, 190, 230, 255))

        # Right Page
        draw.polygon([(140, 32), (210, 32), (224, 46), (224, 224), (140, 224)], fill=white)
        draw.polygon([(210, 32), (210, 46), (224, 46)], fill=(200, 190, 230, 255))

        # Center Gap (x=116 to 140 -> center 128)
        # Gold Outward Split Arrows ◄ ►
        draw.polygon([(96, 128), (114, 104), (114, 152)], fill=c_gold)
        draw.polygon([(160, 128), (142, 104), (142, 152)], fill=c_gold)

        # Central vertical dashed dividing line
        for y in range(32, 224, 16):
            draw.line([(128, y), (128, y + 8)], fill=c_gold, width=3)

    elif action == "extract":
        draw.polygon([(45, 24), (165, 24), (205, 64), (205, 224), (45, 224)], fill=white)
        draw.line([(78, 172), (166, 84)], fill=c_orange, width=26)
        draw.polygon([(178, 72), (118, 78), (172, 132)], fill=c_orange)

    elif action == "rotate":
        # White Document + Cyan Circular Arrow INSIDE the page (High Contrast, no merging)
        draw.polygon([(45, 24), (165, 24), (205, 64), (205, 224), (45, 224)], fill=white)
        draw.polygon([(165, 24), (165, 64), (205, 64)], fill=(200, 210, 230, 255))
        # Cyan Circular Arrow inside
        draw.arc([72, 76, 178, 182], start=30, end=300, fill=c_cyan, width=22)
        draw.polygon([(182, 92), (142, 64), (148, 118)], fill=c_cyan)

    elif action == "text":
        draw.polygon([(45, 24), (165, 24), (205, 64), (205, 224), (45, 224)], fill=white)
        try:
            font = ImageFont.truetype("georgiab.ttf", 140)
        except OSError:
            font = get_font(140, bold=True)
        draw.text((85, 38), "T", fill=c_teal, font=font)

    elif action == "optimize":
        bolt = [(145, 20), (75, 135), (125, 135), (95, 235), (185, 110), (135, 110)]
        draw.polygon(bolt, fill=c_gold, outline=white, width=8)

    elif action == "stamp":
        # White outer circle badge
        draw.ellipse([30, 30, 226, 226], fill=white)
        # Gold inner circle
        draw.ellipse([46, 46, 210, 210], fill=c_gold)
        # Geometric 5-pointed Star in center
        draw_star(draw, cx=128, cy=128, r_outer=68, r_inner=28, fill=white)

    elif action == "convert":
        draw.arc([42, 42, 214, 214], start=20, end=160, fill=white, width=24)
        draw.polygon([(204, 140), (230, 88), (172, 88)], fill=white)
        draw.arc([42, 42, 214, 214], start=200, end=340, fill=white, width=24)
        draw.polygon([(52, 116), (26, 168), (84, 168)], fill=white)

    elif action == "inspect":
        draw.polygon([(45, 24), (165, 24), (205, 64), (205, 224), (45, 224)], fill=white)
        draw.polygon([(165, 24), (165, 64), (205, 64)], fill=(200, 210, 230, 255))
        for y in (75, 105, 135, 165):
            draw.line([(70, y), (140, y)], fill=(180, 190, 200, 255), width=8)
        draw.ellipse([110, 110, 185, 185], outline=c_cyan, width=16)
        draw.line([(165, 165), (205, 205)], fill=c_gold, width=20)

    elif action == "resize":
        draw.polygon([(55, 34), (155, 34), (195, 74), (195, 214), (55, 214)], fill=white)
        draw.polygon([(155, 34), (155, 74), (195, 74)], fill=(200, 210, 230, 255))
        draw.line([(80, 168), (170, 78)], fill=c_cyan, width=16)
        draw.polygon([(178, 70), (130, 75), (173, 118)], fill=c_cyan)
        draw.polygon([(72, 176), (120, 171), (77, 128)], fill=c_cyan)

    elif action == "metadata":
        draw.polygon([(45, 24), (165, 24), (205, 64), (205, 224), (45, 224)], fill=white)
        draw.polygon([(165, 24), (165, 64), (205, 64)], fill=(200, 210, 230, 255))
        draw.polygon([(70, 110), (170, 110), (190, 145), (170, 180), (70, 180)], fill=c_gold)
        draw.ellipse([85, 133, 109, 157], fill=white)

    elif action == "doctor":
        draw.polygon([(45, 24), (165, 24), (205, 64), (205, 224), (45, 224)], fill=white)
        draw.polygon([(165, 24), (165, 64), (205, 64)], fill=(200, 210, 230, 255))
        draw.polygon([(128, 90), (185, 110), (185, 165), (128, 205), (71, 165), (71, 110)], fill=c_teal)
        # Vector Geometric Checkmark (sharp, perfectly centered, no joint artifacts)
        draw.polygon([(92, 144), (118, 170), (164, 124), (152, 112), (118, 146), (104, 132)], fill=white)

    elif action == "number":
        # Page with text lines and a pink page-number badge at the bottom
        draw.polygon([(45, 24), (165, 24), (205, 64), (205, 224), (45, 224)], fill=white)
        draw.polygon([(165, 24), (165, 64), (205, 64)], fill=(200, 210, 230, 255))
        for y in (60, 88, 116):
            draw.line([(70, y), (150, y)], fill=(180, 190, 200, 255), width=10)
        draw.ellipse([86, 132, 170, 216], fill=c_pink)
        font = get_font(68, bold=True)
        draw.text((128, 174), "1", fill=white, font=font, anchor="mm")

    elif action == "watermark":
        # Page with a big translucent water drop across it
        draw.polygon([(45, 24), (165, 24), (205, 64), (205, 224), (45, 224)], fill=white)
        draw.polygon([(165, 24), (165, 64), (205, 64)], fill=(200, 210, 230, 255))
        drop = (90, 160, 225, 255)
        draw.polygon([(125, 50), (78, 140), (172, 140)], fill=drop)
        draw.ellipse([76, 100, 174, 198], fill=drop)
        draw.ellipse([98, 140, 122, 172], fill=(200, 225, 250, 255))

    elif action == "ocr-inplace":
        # OCR page plus a gold circular "in place" arrow badge
        draw.polygon([(30, 18), (140, 18), (176, 54), (176, 206), (30, 206)], fill=white)
        draw.rectangle([22, 100, 184, 128], fill=(0, 230, 120, 230))
        font = get_font(112, bold=True)
        draw.text((103, 112), "A", fill=(20, 30, 40, 255), font=font, anchor="mm")
        draw.ellipse([134, 134, 246, 246], fill=c_gold)
        draw.arc([156, 156, 224, 224], start=40, end=320, fill=white, width=14)
        draw.polygon([(232, 178), (204, 160), (226, 206)], fill=white)

    elif action == "protect":
        draw_padlock(draw, keyhole=bg_color, opened=False)

    elif action == "unlock":
        draw_padlock(draw, keyhole=bg_color, opened=True)

    elif action == "bpdf":
        c_dark_blue = (18, 57, 184, 255)
        # Clear the background previously drawn
        draw.rectangle([0, 0, 256, 256], fill=(0, 0, 0, 0))
        # Solid dark blue square background (full bleed)
        draw.rectangle([0, 0, 256, 256], fill=c_dark_blue)
        
        # White paper with folded corner (scaled up)
        draw.polygon([(40, 16), (160, 16), (216, 88), (216, 240), (40, 240)], fill=white)
        # Folded corner
        draw.polygon([(160, 16), (160, 88), (216, 88)], fill=(200, 210, 230, 255))
        
        # Top blue line
        draw.rectangle([72, 108, 184, 136], fill=c_dark_blue)
        # Bottom blue line (shorter)
        draw.rectangle([72, 152, 136, 180], fill=c_dark_blue)
        
        # Orange square
        draw.rectangle([152, 164, 184, 196], fill=c_orange)
    return img

def main():
    os.makedirs("resources", exist_ok=True)

    icons_data = [
        "merge", "ocr", "strip", "split", "extract", "inspect", "rotate", "resize", "text", "doctor", "stamp", "optimize", "metadata", "convert",
        "number", "watermark", "ocr-inplace", "protect", "unlock"
    ]
    sizes_buttons = [(16, 16), (24, 24), (32, 32)]
    for name in icons_data:
        img = create_icon_perfect(name)
        out_path = os.path.join("resources", f"{name}.ico")
        img.save(out_path, format="ICO", sizes=sizes_buttons)
        print(f"Generated {out_path}")

    # Generate main application icon with all required sizes
    img_main = create_icon_perfect("bpdf")
    sizes_main = [(16, 16), (24, 24), (32, 32), (48, 48), (256, 256)]
    out_main_path = os.path.join("resources", "bpdf.ico")
    img_main.save(out_main_path, format="ICO", sizes=sizes_main)
    print(f"Generated {out_main_path}")

if __name__ == "__main__":
    main()
