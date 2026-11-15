"""Static registration parsing tests; vendor code is never imported."""
import tempfile
import unittest
from pathlib import Path

from vendor_inventory import inventory


class VendorInventoryTests(unittest.TestCase):
    # Verify literal and named ids are both registered
    def test_literal_and_named_ids_are_both_registered(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "Python-SDK/src/StreamDock/ProductIDs.py"
            source.parent.mkdir(parents=True)
            source.write_text(
                "class Vendor:\n    M18 = 0x6603\n"
                "class Product:\n    M18 = 0x1009\n"
                "g_products = [\n"
                "    (Vendor.M18, Product.M18, StreamDockM18),\n"
                "    (0x5548, 0x1000, StreamDockM18),\n"
                "]\n"
            )
            (root / "CPP-SDK/src/HotspotDevice").mkdir(parents=True)
            result = inventory(root)
            self.assertEqual(
                [row["usb_id"] for row in result["python_registrations"]],
                ["6603:1009", "5548:1000"],
            )
            self.assertEqual(result["python_only"], ["5548:1000", "6603:1009"])
            self.assertEqual(result["cpp_only"], [])


if __name__ == "__main__":
    unittest.main()
