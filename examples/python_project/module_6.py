"""Example python module 6 for indexing tests."""

def mutate_obj_6(obj):
    obj = obj + 6
    return obj

class Worker6:
    def apply(self, value):
        return mutate_obj_6(value)
