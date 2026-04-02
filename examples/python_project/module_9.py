"""Example python module 9 for indexing tests."""

def mutate_obj_9(obj):
    obj = obj + 9
    return obj

class Worker9:
    def apply(self, value):
        return mutate_obj_9(value)
